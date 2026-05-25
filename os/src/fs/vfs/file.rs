//! File 结构体 — VFS 层的文件描述符抽象
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/file.rs` 中的 `File`。
//! 负责管理：文件偏移量、打开标志、访问模式、文件类型等 per-fd 状态。
//!
//! 与 `IndexNode` 的关系：
//! - `File` 存储 per-fd 可变状态（offset、flags、mode）
//! - `IndexNode` 存储 per-inode 共享状态（数据块、元数据）
//! - `File::read()` 调用 `IndexNode::read_at()`，然后更新 offset

use crate::utils::error::SyscallErr;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::{Mutex, MutexGuard};

use super::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use super::event::EventWaitQueue;
use crate::task::WaitQueue;
use crate::config::SYSTEM_FD_LIMIT;

pub const F_SEAL_SEAL: usize = 0x0001;
pub const F_SEAL_SHRINK: usize = 0x0002;
pub const F_SEAL_GROW: usize = 0x0004;
pub const F_SEAL_WRITE: usize = 0x0008;
pub const F_SEAL_FUTURE_WRITE: usize = 0x0010;

// ── FileFlags ───────────────────────────────────────────────────────────

bitflags! {
    /// 文件打开标志
    pub struct FileFlags: u32 {
        // 访问模式
        const O_RDONLY  = 0o0;
        const O_WRONLY  = 0o1;
        const O_RDWR    = 0o2;
        const O_ACCMODE = 0o3;

        // 打开时标志
        const O_CREAT   = 0o0100;
        const O_EXCL    = 0o0200;
        const O_NOCTTY  = 0o0400;
        const O_TRUNC   = 0o1000;

        // 文件状态标志
        const O_APPEND    = 0o2000;
        const O_NONBLOCK  = 0o4000;
        const O_DSYNC     = 0o10000;
        const O_SYNC      = 0o4010000;
        const O_RSYNC     = 0o4010000;
        const O_DIRECTORY = 0o200000;
        const O_NOFOLLOW  = 0o400000;
        const O_CLOEXEC   = 0o2000000;
        const O_ASYNC     = 0o20000;
        const O_DIRECT    = 0o40000;
        const O_LARGEFILE = 0o100000;
        const O_NOATIME   = 0o1000000;
        const O_PATH      = 0o10000000;
        const O_TMPFILE   = 0o20200000;
    }
}

impl FileFlags {
    #[inline]
    pub fn access_flags(&self) -> FileFlags {
        *self & Self::O_ACCMODE
    }

    #[inline]
    pub fn is_read_only(&self) -> bool {
        self.access_flags() == Self::O_RDONLY
    }

    #[inline]
    pub fn is_write_only(&self) -> bool {
        self.access_flags() == Self::O_WRONLY
    }

    #[inline]
    pub fn is_read_write(&self) -> bool {
        self.access_flags() == Self::O_RDWR
    }

    #[inline]
    pub fn is_readable(&self) -> bool {
        self.access_flags() != Self::O_WRONLY
    }

    #[inline]
    pub fn is_writable(&self) -> bool {
        self.access_flags() == Self::O_WRONLY || self.access_flags() == Self::O_RDWR
    }
}

// ── FileMode ────────────────────────────────────────────────────────────

bitflags! {
    /// 文件访问模式（内部使用，从 flags 推导）
    pub struct FileMode: u32 {
        /// 以读模式打开
        const FMODE_READ  = 0x1;
        /// 以写模式打开
        const FMODE_WRITE = 0x2;
        /// 支持 lseek
        const FMODE_LSEEK = 0x4;
        /// 支持 pread
        const FMODE_PREAD = 0x8;
        /// 支持 pwrite
        const FMODE_PWRITE = 0x10;
        /// O_PATH 文件（几乎不能做 I/O）
        const FMODE_PATH  = 0x20;
        /// 流式文件（pipe/socket），pread/pwrite 返回 ESPIPE
        const FMODE_STREAM = 0x40;
        /// 支持随机访问
        const FMODE_RANDOM = 0x80;
    }
}

// ── SeekFrom ────────────────────────────────────────────────────────────

/// seek 操作的起始位置
#[derive(Debug, Clone, Copy)]
pub enum SeekFrom {
    SeekSet(i64),
    SeekCurrent(i64),
    SeekEnd(i64),
}

// ── FdTable ─────────────────────────────────────────────────────────────

/// 文件描述符表
///
/// 对标 DragonOS 的 `FileDescriptorVec`。
/// 每个进程有一个 `FdTable`，存储所有打开的文件描述符。
#[derive(Debug)]
pub struct FdTable {
    /// 文件描述符数组
    fds: Vec<Option<File>>,
    /// per-fd 的 close_on_exec 标志
    cloexec: Vec<bool>,
    /// 下一个可用的 fd（优化分配，避免 O(n²)）
    next_fd: usize,
    /// 当前软限制
    soft_limit: usize,
    /// 硬限制
    hard_limit: usize,
}

impl FdTable {
    const INITIAL_CAPACITY: usize = 32;
    const MAX_CAPACITY: usize = SYSTEM_FD_LIMIT;

    /// 创建一个新的 FdTable
    pub fn new() -> Self {
        let capacity = Self::INITIAL_CAPACITY;
        let mut fds = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            fds.push(None);
        }
        let mut cloexec = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            cloexec.push(false);
        }

        FdTable {
            fds,
            cloexec,
            next_fd: 0,
            soft_limit: Self::MAX_CAPACITY,
            hard_limit: Self::MAX_CAPACITY,
        }
    }

    /// 释放内部 Vec 的 backing storage，替换为零容量的空 Vec。
    /// 用于 zombie 进程退出时释放 fd 表占用的堆内存。
    pub fn release_backing_storage(&mut self) {
        self.fds = alloc::vec::Vec::new();
        self.cloexec = alloc::vec::Vec::new();
        self.next_fd = 0;
    }

    /// 克隆 FdTable（fork 时用）
    pub fn try_clone(&self) -> Result<Self, SyscallErr> {
        let hi = self.highest_open_index().map(|i| i + 1).unwrap_or(0);
        let clone_len = hi.max(Self::INITIAL_CAPACITY).min(self.fds.len());

        let mut fds = Vec::new();
        if fds.try_reserve(clone_len).is_err() {
            return Err(SyscallErr::ENOMEM);
        }
        fds.extend(
            self.fds[..clone_len]
                .iter()
                .map(|opt| opt.as_ref().and_then(|f| f.try_clone())),
        );

        let mut cloexec = Vec::new();
        if cloexec.try_reserve(clone_len).is_err() {
            return Err(SyscallErr::ENOMEM);
        }
        cloexec.extend(self.cloexec[..clone_len].iter().copied());

        Ok(FdTable {
            fds,
            cloexec,
            next_fd: 0,
            soft_limit: self.soft_limit,
            hard_limit: self.hard_limit,
        })
    }

    // ── 容量管理 ──────────────────────────────────────────────────

    /// 确保容量至少为 `desired`
    fn resize_to_capacity(&mut self, new_capacity: usize) -> Result<(), SyscallErr> {
        if new_capacity > Self::MAX_CAPACITY {
            return Err(SyscallErr::EMFILE);
        }
        let current_len = self.fds.len();
        if new_capacity > current_len {
            self.fds
                .try_reserve(new_capacity - current_len)
                .map_err(|_| SyscallErr::ENOMEM)?;
            self.cloexec
                .try_reserve(new_capacity - current_len)
                .map_err(|_| SyscallErr::ENOMEM)?;
            for _ in 0..(new_capacity - current_len) {
                self.fds.push(None);
                self.cloexec.push(false);
            }
        } else if new_capacity < current_len {
            let floor = self.highest_open_index().map(|i| i + 1).unwrap_or(0);
            let target = core::cmp::max(new_capacity, floor);
            if target < current_len {
                self.fds.truncate(target);
                self.cloexec.truncate(target);
                if self.next_fd > target {
                    self.next_fd = target;
                }
            }
        }
        Ok(())
    }

    fn highest_open_index(&self) -> Option<usize> {
        self.fds.iter().rposition(|f| f.is_some())
    }

    fn effective_soft_limit(&self) -> usize {
        self.soft_limit.min(Self::MAX_CAPACITY)
    }

    // ── FD 分配/释放 ──────────────────────────────────────────────

    /// 分配一个新的文件描述符
    pub fn alloc_fd(&mut self, file: File, cloexec: bool) -> Result<usize, SyscallErr> {
        let limit = self.effective_soft_limit();
        if limit == 0 {
            return Err(SyscallErr::EMFILE);
        }

        // Linux open/pipe/socket 语义要求返回最低编号的可用 fd。
        let len = self.fds.len().min(limit);
        for i in 0..len {
            if self.fds[i].is_none() {
                self.fds[i] = Some(file);
                self.cloexec[i] = cloexec;
                self.next_fd = i + 1;
                return Ok(i);
            }
        }

        // 没有空闲的，尝试扩容
        let old_capacity = self.fds.len();
        if old_capacity >= limit {
            return Err(SyscallErr::EMFILE);
        }
        let new_capacity = core::cmp::min(old_capacity.saturating_mul(2).max(old_capacity + 1), limit);
        self.resize_to_capacity(new_capacity)?;
        // 递归分配
        self.alloc_fd(file, cloexec)
    }

    /// 分配不小于 `min_fd` 的空闲 fd（fcntl F_DUPFD/F_DUPFD_CLOEXEC）。
    pub fn alloc_fd_from(
        &mut self,
        min_fd: usize,
        file: File,
        cloexec: bool,
    ) -> Result<usize, SyscallErr> {
        let limit = self.effective_soft_limit();
        if min_fd >= limit {
            return Err(SyscallErr::EINVAL);
        }

        loop {
            let len = self.fds.len().min(limit);
            for fd in min_fd..len {
                if self.fds[fd].is_none() {
                    self.fds[fd] = Some(file);
                    self.cloexec[fd] = cloexec;
                    if fd < self.next_fd {
                        self.next_fd = fd + 1;
                    }
                    return Ok(fd);
                }
            }

            let old_capacity = self.fds.len();
            if old_capacity >= limit {
                return Err(SyscallErr::EMFILE);
            }
            let wanted = core::cmp::max(min_fd + 1, old_capacity + 1);
            let doubled = old_capacity.saturating_mul(2).max(wanted);
            self.resize_to_capacity(core::cmp::min(doubled, limit))?;
        }
    }

    /// 在指定位置分配 fd（dup2 用）
    pub fn alloc_fd_at(
        &mut self,
        fd: usize,
        file: File,
        cloexec: bool,
    ) -> Result<usize, SyscallErr> {
        if fd >= self.effective_soft_limit() {
            return Err(SyscallErr::EBADF);
        }
        // 扩容到至少 fd + 1
        while self.fds.len() <= fd {
            let new_cap = core::cmp::min(self.fds.len() * 2, self.effective_soft_limit());
            if new_cap <= self.fds.len() {
                return Err(SyscallErr::EMFILE);
            }
            self.resize_to_capacity(new_cap)?;
        }
        self.fds[fd] = Some(file);
        self.cloexec[fd] = cloexec;
        Ok(fd)
    }

    /// 释放一个 fd
    pub fn drop_fd(&mut self, fd: usize) -> Result<File, SyscallErr> {
        if fd >= self.fds.len() {
            return Err(SyscallErr::EBADF);
        }
        let file = self.fds[fd].take().ok_or(SyscallErr::EBADF)?;
        self.cloexec[fd] = false;
        if fd < self.next_fd {
            self.next_fd = fd;
        }
        Ok(file)
    }

    // ── FD 访问 ───────────────────────────────────────────────────

    /// 获取 fd 对应的 File 引用
    #[inline]
    pub fn get_file(&self, fd: usize) -> Result<&File, SyscallErr> {
        if fd >= self.fds.len() {
            return Err(SyscallErr::EBADF);
        }
        self.fds[fd].as_ref().ok_or(SyscallErr::EBADF)
    }

    /// 获取 fd 对应的 File 可变引用
    #[inline]
    pub fn get_file_mut(&mut self, fd: usize) -> Result<&mut File, SyscallErr> {
        if fd >= self.fds.len() {
            return Err(SyscallErr::EBADF);
        }
        self.fds[fd].as_mut().ok_or(SyscallErr::EBADF)
    }

    /// 获取 close_on_exec 标志
    #[inline]
    pub fn get_cloexec(&self, fd: usize) -> bool {
        if fd >= self.cloexec.len() {
            return false;
        }
        self.cloexec[fd]
    }

    /// 设置 close_on_exec 标志
    #[inline]
    pub fn set_cloexec(&mut self, fd: usize, val: bool) -> Result<(), SyscallErr> {
        if fd >= self.cloexec.len() {
            return Err(SyscallErr::EBADF);
        }
        self.cloexec[fd] = val;
        Ok(())
    }

    pub fn close_range(&mut self, first: usize, last: usize) {
        if first >= self.fds.len() {
            return;
        }
        let last = last.min(self.fds.len().saturating_sub(1));
        for fd in first..=last {
            self.fds[fd] = None;
            self.cloexec[fd] = false;
        }
        if first < self.next_fd {
            self.next_fd = first;
        }
    }

    pub fn set_cloexec_range(&mut self, first: usize, last: usize) {
        if first >= self.fds.len() {
            return;
        }
        let last = last.min(self.fds.len().saturating_sub(1));
        for fd in first..=last {
            if self.fds[fd].is_some() {
                self.cloexec[fd] = true;
            }
        }
    }

    /// 遍历所有打开的 fd
    pub fn iter(&self) -> impl Iterator<Item = (usize, &File)> {
        self.fds
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.as_ref().map(|f| (i, f)))
    }

    /// 获取 fd 数量
    pub fn fd_count(&self) -> usize {
        self.fds.iter().filter(|f| f.is_some()).count()
    }

    /// 获取 FdTable 的容量（最大可能的 fd 索引 + 1）
    pub fn len(&self) -> usize {
        self.fds.len()
    }

    /// 获取底层 Vec 的 capacity（堆分配大小）
    pub fn capacity(&self) -> usize {
        self.fds.capacity()
    }

    // ── exec 相关 ─────────────────────────────────────────────────

    /// exec 时：关闭所有 CLOEXEC 的 fd
    pub fn close_cloexec(&mut self) {
        for (i, cloexec) in self.cloexec.iter().enumerate() {
            if *cloexec {
                self.fds[i] = None;
            }
        }
        self.cloexec.fill(false);
        self.next_fd = 0;
    }

    // ── 过渡桥接（syscall 迁移期间使用 vfs::File）────────────────

    pub fn get_ref(&self, fd: usize) -> Result<&File, isize> {
        self.get_file(fd).map_err(|e| -(e as isize))
    }

    pub fn get_refmut(&mut self, fd: usize) -> Result<&mut File, isize> {
        self.get_file_mut(fd).map_err(|e| -(e as isize))
    }

    pub fn remove(&mut self, fd: usize) -> Result<File, isize> {
        self.drop_fd(fd).map_err(|e| -(e as isize))
    }

    pub fn insert(&mut self, file: File) -> Result<usize, isize> {
        self.alloc_fd(file, false).map_err(|e| -(e as isize))
    }

    pub fn insert_at(&mut self, file: File, pos: usize) -> Result<usize, isize> {
        self.alloc_fd_at(pos, file, false).map_err(|e| -(e as isize))
    }

    pub fn try_insert_at(&mut self, file: File, hint: usize) -> Result<usize, isize> {
        self.insert_at(file, hint)
    }

    pub fn check(&self, fd: usize) -> Result<(), isize> {
        self.get_file(fd).map(|_| ()).map_err(|e| -(e as isize))
    }

    pub fn get_soft_limit(&self) -> usize { self.soft_limit }
    pub fn set_soft_limit(&mut self, limit: usize) {
        self.soft_limit = limit.min(Self::MAX_CAPACITY);
    }
    pub fn get_hard_limit(&self) -> usize { self.hard_limit }
    pub fn set_hard_limit(&mut self, limit: usize) {
        self.hard_limit = limit.min(Self::MAX_CAPACITY);
    }
}

impl Drop for FdTable {
    fn drop(&mut self) {
        // 所有 File 的 drop 会触发 inode 的 close
        self.fds.clear();
    }
}

// ── File ────────────────────────────────────────────────────────────────

/// 文件结构体
///
/// 封装一个 `IndexNode`，管理 per-fd 状态。
/// 对标 DragonOS `kernel/src/filesystem/vfs/file.rs` 的 `File`。
pub struct File {
    /// 对应的 inode
    pub inode: Arc<dyn IndexNode>,
    /// 文件偏移量（Arc 确保 clone/dup 后与源文件共享偏移量）
    offset: Arc<AtomicUsize>,
    /// 打开标志
    flags: Mutex<FileFlags>,
    /// 文件访问模式
    mode: Mutex<FileMode>,
    /// 文件类型
    file_type: FileType,
    /// 私有数据
    private_data: Mutex<FilePrivateData>,
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("File")
            .field("offset", &self.offset.load(Ordering::Relaxed))
            .field("flags", &self.flags)
            .field("mode", &self.mode)
            .field("file_type", &self.file_type)
            .finish()
    }
}

pub struct PollWaitQueue {
    _inode: Arc<dyn IndexNode>,
    queue: *const Mutex<WaitQueue>,
}

impl PollWaitQueue {
    pub fn queue(&self) -> &Mutex<WaitQueue> {
        // `PollWaitQueue` keeps the inode Arc alive, so the queue reference
        // returned by `IndexNode` remains valid for this poll wait cycle.
        unsafe { &*self.queue }
    }
}

#[derive(Clone)]
pub struct EventQueueHandle {
    _inode: Arc<dyn IndexNode>,
    queue: *const EventWaitQueue,
}

unsafe impl Send for EventQueueHandle {}
unsafe impl Sync for EventQueueHandle {}

impl EventQueueHandle {
    pub fn queue(&self) -> &EventWaitQueue {
        // `EventQueueHandle` keeps the inode Arc alive, so the queue reference
        // returned by `IndexNode` remains valid for this poll/epoll cycle.
        unsafe { &*self.queue }
    }
}

impl File {
    /// 根据 inode 创建新 File
    pub fn new(inode: Arc<dyn IndexNode>, flags: FileFlags) -> Result<Self, SyscallErr> {
        // 推导 mode
        let mut mode = FileMode::FMODE_LSEEK | FileMode::FMODE_PREAD | FileMode::FMODE_PWRITE;

        if flags.is_readable() {
            mode |= FileMode::FMODE_READ;
        }
        if flags.is_writable() {
            mode |= FileMode::FMODE_WRITE;
        }
        if flags.contains(FileFlags::O_PATH) {
            mode |= FileMode::FMODE_PATH;
        }

        let metadata = match inode.metadata() {
            Ok(m) => m,
            Err(_) => {
                // 某些特殊 inode（如 socket 创建时）可能还没有完整 metadata
                // 尝试创建默认 metadata
                Metadata::new(FileType::File, InodeMode::S_IRWXUGO)
            }
        };

        let file_type = metadata.file_type;

        // 对于流式文件（pipe/socket），设置 FMODE_STREAM
        if matches!(file_type, FileType::Pipe | FileType::Socket) || inode.is_stream() {
            mode |= FileMode::FMODE_STREAM;
        }

        // 调用 inode 的 open
        let private_data = FilePrivateData::default();
        let file = File {
            inode,
            offset: Arc::new(AtomicUsize::new(0)),
            flags: Mutex::new(flags),
            mode: Mutex::new(mode),
            file_type,
            private_data: Mutex::new(private_data),
        };

        file.inode.open(file.private_data.lock(), &flags)?;

        Ok(file)
    }

    /// 创建新 File，不调用 inode.open（用于 socket create 等场景）
    pub fn new_without_open(
        inode: Arc<dyn IndexNode>,
        flags: FileFlags,
        file_type: FileType,
    ) -> Self {
        let mut mode = FileMode::FMODE_LSEEK | FileMode::FMODE_PREAD | FileMode::FMODE_PWRITE;
        if flags.is_readable() {
            mode |= FileMode::FMODE_READ;
        }
        if flags.is_writable() {
            mode |= FileMode::FMODE_WRITE;
        }
        if matches!(file_type, FileType::Pipe | FileType::Socket) || inode.is_stream() {
            mode |= FileMode::FMODE_STREAM;
        }

        File {
            inode,
            offset: Arc::new(AtomicUsize::new(0)),
            flags: Mutex::new(flags),
            mode: Mutex::new(mode),
            file_type,
            private_data: Mutex::new(FilePrivateData::default()),
        }
    }

    // ── 读取 ───────────────────────────────────────────────────────

    /// 从文件当前位置读取（并推进 offset）
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.readable()?;
        let offset = self.offset.load(Ordering::SeqCst);
        let len = buf.len();

        if len == 0 {
            return Ok(0);
        }

        let n = self
            .inode
            .read_at(offset, len, buf, self.private_data.lock())?;

        if n > 0 {
            self.offset.fetch_add(n, Ordering::SeqCst);
        }
        Ok(n)
    }

    /// 从指定位置读取（不推进 offset）
    pub fn pread(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        let mode = *self.mode.lock();
        if mode.contains(FileMode::FMODE_PATH) {
            return Err(SyscallErr::EBADF);
        }
        if mode.contains(FileMode::FMODE_STREAM) {
            return Err(SyscallErr::ESPIPE);
        }
        self.inode
            .read_at(offset, buf.len(), buf, self.private_data.lock())
    }

    // ── 写入 ───────────────────────────────────────────────────────

    /// 从文件当前位置写入（并推进 offset）
    pub fn write(&self, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.writable()?;
        let flags = *self.flags.lock();
        let len = buf.len();

        if len == 0 {
            return Ok(0);
        }

        let offset = if flags.contains(FileFlags::O_APPEND) {
            // O_APPEND: 写入到文件末尾
            let md = self.inode.metadata()?;
            md.size.max(0) as usize
        } else {
            self.offset.load(Ordering::SeqCst)
        };
        self.check_memfd_write_seals(offset, len)?;

        let n = self
            .inode
            .write_at(offset, len, buf, self.private_data.lock())?;

        if !flags.contains(FileFlags::O_APPEND) && n > 0 {
            self.offset.fetch_add(n, Ordering::SeqCst);
        }
        if n > 0 {
            self.touch_modified();
        }
        Ok(n)
    }

    /// 从指定位置写入（不推进 offset）
    pub fn pwrite(&self, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        let mode = *self.mode.lock();
        if mode.contains(FileMode::FMODE_PATH) {
            return Err(SyscallErr::EBADF);
        }
        if mode.contains(FileMode::FMODE_STREAM) {
            return Err(SyscallErr::ESPIPE);
        }
        let flags = *self.flags.lock();
        let offset = if flags.contains(FileFlags::O_APPEND) {
            let md = self.inode.metadata()?;
            md.size.max(0) as usize
        } else {
            offset
        };
        self.check_memfd_write_seals(offset, buf.len())?;
        let n = self
            .inode
            .write_at(offset, buf.len(), buf, self.private_data.lock())?;
        if n > 0 {
            self.touch_modified();
        }
        Ok(n)
    }

    // ── Seek ───────────────────────────────────────────────────────

    /// 调整文件偏移量
    pub fn lseek(&self, whence: SeekFrom) -> Result<usize, SyscallErr> {
        let mode = *self.mode.lock();
        if mode.contains(FileMode::FMODE_STREAM) {
            return Err(SyscallErr::ESPIPE);
        }
        if !mode.contains(FileMode::FMODE_LSEEK) {
            return Err(SyscallErr::ESPIPE);
        }

        let pos: i64 = match whence {
            SeekFrom::SeekSet(offset) => offset,
            SeekFrom::SeekCurrent(offset) => self.offset.load(Ordering::SeqCst) as i64 + offset,
            SeekFrom::SeekEnd(offset) => {
                // 对目录，允许 SEEK_END
                let md = self.inode.metadata()?;
                md.size + offset
            }
        };

        if pos < 0 {
            return Err(SyscallErr::EINVAL);
        }

        self.offset.store(pos as usize, Ordering::SeqCst);
        Ok(pos as usize)
    }

    // ── 权限检查 ───────────────────────────────────────────────────

    #[inline]
    pub fn readable(&self) -> Result<(), SyscallErr> {
        let mode = *self.mode.lock();
        if mode.contains(FileMode::FMODE_PATH) {
            return Err(SyscallErr::EBADF);
        }
        if !mode.contains(FileMode::FMODE_READ) {
            return Err(SyscallErr::EBADF);
        }
        Ok(())
    }

    #[inline]
    pub fn writable(&self) -> Result<(), SyscallErr> {
        let mode = *self.mode.lock();
        if mode.contains(FileMode::FMODE_PATH) {
            return Err(SyscallErr::EBADF);
        }
        if !mode.contains(FileMode::FMODE_WRITE) {
            return Err(SyscallErr::EBADF);
        }
        Ok(())
    }

    /// 读就绪检查（poll 用）
    pub fn r_ready(&self) -> bool {
        self.poll_events()
            .intersects(super::event::EPollEvent::EPOLLIN | super::event::EPollEvent::EPOLLRDNORM)
    }

    /// 写就绪检查（poll 用）
    pub fn w_ready(&self) -> bool {
        self.poll_events().intersects(
            super::event::EPollEvent::EPOLLOUT | super::event::EPollEvent::EPOLLWRNORM,
        )
    }

    /// 统一 poll 事件检查。
    ///
    /// 还没有实现 poll 的普通文件保持旧行为：默认读写均就绪。
    pub fn poll_events(&self) -> super::event::EPollEvent {
        match self.inode.poll(&*self.private_data.lock()) {
            Ok(revents) => super::event::EPollEvent::from_bits_truncate(revents),
            Err(_) => super::event::EPollEvent::EPOLLIN
                | super::event::EPollEvent::EPOLLOUT
                | super::event::EPollEvent::EPOLLRDNORM
                | super::event::EPollEvent::EPOLLWRNORM,
        }
    }

    pub fn read_wait_queue(&self) -> Option<PollWaitQueue> {
        if let Some(queue) = self.inode.read_event_queue() {
            return Some(PollWaitQueue {
                _inode: self.inode.clone(),
                queue: queue.wait_queue() as *const Mutex<WaitQueue>,
            });
        }
        let queue = self.inode.read_wait_queue()? as *const Mutex<WaitQueue>;
        Some(PollWaitQueue {
            _inode: self.inode.clone(),
            queue,
        })
    }

    pub fn write_wait_queue(&self) -> Option<PollWaitQueue> {
        if let Some(queue) = self.inode.write_event_queue() {
            return Some(PollWaitQueue {
                _inode: self.inode.clone(),
                queue: queue.wait_queue() as *const Mutex<WaitQueue>,
            });
        }
        let queue = self.inode.write_wait_queue()? as *const Mutex<WaitQueue>;
        Some(PollWaitQueue {
            _inode: self.inode.clone(),
            queue,
        })
    }

    pub fn read_event_queue(&self) -> Option<EventQueueHandle> {
        let queue = self.inode.read_event_queue()? as *const EventWaitQueue;
        Some(EventQueueHandle {
            _inode: self.inode.clone(),
            queue,
        })
    }

    pub fn write_event_queue(&self) -> Option<EventQueueHandle> {
        let queue = self.inode.write_event_queue()? as *const EventWaitQueue;
        Some(EventQueueHandle {
            _inode: self.inode.clone(),
            queue,
        })
    }

    // ── 属性访问 ───────────────────────────────────────────────────

    #[inline]
    pub fn flags(&self) -> FileFlags {
        *self.flags.lock()
    }

    #[inline]
    pub fn set_flags(&self, new_flags: FileFlags) -> Result<(), SyscallErr> {
        // 访问模式不可修改
        let old_flags = self.flags();
        if old_flags.access_flags() != new_flags.access_flags() {
            return Err(SyscallErr::EINVAL);
        }
        const SETFL_MASK: u32 = FileFlags::O_APPEND.bits()
            | FileFlags::O_NONBLOCK.bits()
            | FileFlags::O_DIRECT.bits()
            | FileFlags::O_NOATIME.bits()
            | FileFlags::O_ASYNC.bits();
        let new_bits = old_flags.bits() & !SETFL_MASK | new_flags.bits() & SETFL_MASK;
        *self.flags.lock() = FileFlags::from_bits_truncate(new_bits);
        Ok(())
    }

    #[inline]
    pub fn mode(&self) -> FileMode {
        *self.mode.lock()
    }

    #[inline]
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    #[inline]
    pub fn metadata(&self) -> Result<Metadata, SyscallErr> {
        self.inode.metadata()
    }

    fn touch_modified(&self) {
        let Ok(mut metadata) = self.inode.metadata() else {
            return;
        };
        let now = crate::timer::TimeSpec::now();
        metadata.mtime = now;
        metadata.ctime = now;
        let _ = self.inode.set_metadata(&metadata);
    }

    pub fn offset(&self) -> usize {
        self.offset.load(Ordering::SeqCst)
    }

    pub fn set_offset(&self, off: usize) {
        self.offset.store(off, Ordering::SeqCst);
    }

    /// 获取私有数据的锁
    pub fn private_data(&self) -> MutexGuard<FilePrivateData> {
        self.private_data.lock()
    }

    pub fn set_memfd_seals(&self, seals: Arc<AtomicUsize>) {
        *self.private_data.lock() = FilePrivateData::Memfd { seals };
    }

    pub fn memfd_seals(&self) -> Option<Arc<AtomicUsize>> {
        match &*self.private_data.lock() {
            FilePrivateData::Memfd { seals } => Some(seals.clone()),
            _ => None,
        }
    }

    pub fn memfd_seal_bits(&self) -> Option<usize> {
        self.memfd_seals()
            .map(|seals| seals.load(Ordering::SeqCst))
    }

    fn check_memfd_write_seals(&self, offset: usize, len: usize) -> Result<(), SyscallErr> {
        let Some(seals) = self.memfd_seal_bits() else {
            return Ok(());
        };
        if (seals & F_SEAL_WRITE) != 0 {
            return Err(SyscallErr::EPERM);
        }
        if (seals & F_SEAL_GROW) != 0 {
            let end = offset.checked_add(len).ok_or(SyscallErr::EFBIG)?;
            let size = self.metadata()?.size.max(0) as usize;
            if end > size {
                return Err(SyscallErr::EPERM);
            }
        }
        Ok(())
    }

    pub fn inode_as_any_ref(&self) -> &dyn Any {
        self.inode.as_any_ref()
    }

    // ── Clone ──────────────────────────────────────────────────────

    /// 尝试克隆 File（dup 时用）
    pub fn try_clone(&self) -> Option<Self> {
        let inode = self.inode.clone();
        let flags = *self.flags.lock();
        let private_data = self.private_data.lock().clone();
        Some(File {
            inode,
            offset: Arc::clone(&self.offset),
            flags: Mutex::new(flags),
            mode: Mutex::new(*self.mode.lock()),
            file_type: self.file_type,
            private_data: Mutex::new(private_data),
        })
    }

    /// 获取 O_NONBLOCK 标志
    pub fn is_nonblock(&self) -> bool {
        self.flags().contains(FileFlags::O_NONBLOCK)
    }

    /// 设置 O_NONBLOCK 标志
    pub fn set_nonblock(&self, nonblock: bool) {
        let mut flags = self.flags.lock();
        if nonblock {
            flags.insert(FileFlags::O_NONBLOCK);
        } else {
            flags.remove(FileFlags::O_NONBLOCK);
        }
    }

    /// 是否为目录
    pub fn is_dir(&self) -> bool {
        self.file_type == FileType::Dir
    }

    // ── 内核空间映射 ───────────────────────────────────────────────

    /// 将文件页缓存映射到内核虚拟地址空间，返回对整个文件的切片引用。
    /// 对标旧 `FileDescriptor::map_to_kernel_space`。用于 ELF 加载。
    ///
    /// 当 inode 有 page_cache 时直接使用页缓存物理帧；
    /// 否则（如 ramfs）手动分配帧并从文件读取数据。
    pub fn map_to_kernel_space(&self, base: usize) -> &'static [u8] {
        use crate::mm::{frame_alloc, Frame, FrameTracker, MapPermission, KERNEL_SPACE};
        use crate::config::PAGE_SIZE;
        use core::convert::TryInto;

        let size = self.get_size();
        log::info!("[map_to_kernel_space] size={} inode={:?}", size, self.inode.metadata().ok());
        let need_pages = if size == 0 { 0 } else { (size + PAGE_SIZE - 1) / PAGE_SIZE };

        // Helper: allocate frames + pread full file content into them
        let alloc_and_pread = |size: usize, need_pages: usize| -> Vec<Arc<FrameTracker>> {
            log::debug!(
                "[map_to_kernel_space] allocating {} frames + pread {} bytes (page-at-a-time)",
                need_pages,
                size
            );
            let mut trackers: Vec<Arc<FrameTracker>> = Vec::with_capacity(need_pages);
            for _ in 0..need_pages {
                trackers.push(frame_alloc().expect("map_to_kernel_space: frame_alloc failed"));
            }
            // pread directly into each frame, avoiding a monolithic heap Vec
            let mut offset = 0;
            for tracker in &trackers {
                let dst = tracker.ppn.get_bytes_array();
                let chunk = (size - offset).min(PAGE_SIZE);
                let n = self
                    .pread(offset, &mut dst[..chunk])
                    .expect("map_to_kernel_space: pread failed");
                if n != chunk {
                    log::warn!(
                        "[map_to_kernel_space] pread at offset {} returned {} bytes, expected {}",
                        offset, n, chunk
                    );
                }
                offset += chunk;
            }
            trackers
        };

        let frames: Vec<Arc<FrameTracker>> = if need_pages == 0 {
            Vec::new()
        } else {
            // ELF 加载不能信任 page-cache 快捷路径——
            // frame_trackers() 返回的帧可能只包含部分数据（如 4 字节 magic read
            // 残留），导致 from_elf 拿到残缺的 ELF header 而返回 ENOEXEC。
            // 始终走 alloc_and_pread 确保完整读取文件内容。
            alloc_and_pread(size, need_pages)
        };

        KERNEL_SPACE
            .lock()
            .insert_program_area(
                crate::mm::VirtAddr::from(base).try_into().unwrap(),
                MapPermission::R | MapPermission::W,
                frames,
            )
            .unwrap();

        unsafe { core::slice::from_raw_parts_mut(base as *mut u8, size) }
    }

    /// 获取文件大小
    pub fn get_size(&self) -> usize {
        self.inode.metadata().map(|m| m.size as usize).unwrap_or(0)
    }

    /// 截断
    pub fn truncate_size(&self, new_size: usize) -> Result<(), SyscallErr> {
        self.inode.resize(new_size)
    }

    /// 获取目录项
    pub fn get_dirent(&self, count: usize) -> Result<Vec<crate::fs::dirent::Dirent>, isize> {
        if !self.is_dir() {
            return Err(crate::syscall::errno::ENOTDIR);
        }
        let names = self
            .inode
            .list()
            .map_err(|_| crate::syscall::errno::ENOSYS)?;

        let dirent_size = core::mem::size_of::<crate::fs::dirent::Dirent>();
        let offset = self.offset.load(Ordering::SeqCst);
        let start_index = offset / dirent_size;

        if start_index >= names.len() {
            return Ok(Vec::new());
        }

        let mut dirents: Vec<crate::fs::dirent::Dirent> = names
            .iter()
            .skip(start_index)
            .take(count)
            .enumerate()
            .map(|(i, name)| {
                let (d_type, d_ino) = match self.inode.find(name) {
                    Ok(child) => match child.metadata() {
                        Ok(m) => {
                            let dt = match m.file_type {
                                FileType::Dir => 4,       // DT_DIR
                                FileType::File => 8,       // DT_REG
                                FileType::SymLink => 10,   // DT_LNK
                                FileType::CharDevice => 2, // DT_CHR
                                FileType::BlockDevice => 6,// DT_BLK
                                FileType::Pipe => 5,       // DT_FIFO
                                FileType::Socket => 12,    // DT_SOCK
                                _ => 0,                    // DT_UNKNOWN
                            };
                            (dt, m.inode_id)
                        }
                        Err(_) => (0, 0),
                    },
                    Err(_) => (0, 0),
                };
                // d_off = 下一个条目的偏移量，Linux 语义
                let d_off = ((start_index + i + 1) * dirent_size) as isize;
                crate::fs::dirent::Dirent::new(d_ino, d_off, d_type, name)
            })
            .collect();

        // 更新文件偏移量，确保下一次 getdents64 从正确位置继续
        let new_offset = (start_index + dirents.len()) * dirent_size;
        self.offset.store(new_offset, Ordering::SeqCst);

        Ok(dirents)
    }

    /// 将目录项打包为变长 linux_dirent64 记录写入 `buf`。
    ///
    /// 布局：d_ino(u64) + d_off(i64) + d_reclen(u16) + d_name(NUL 结尾)，
    /// d_type 按 Linux 语义写在记录最后一个字节，整体 8 字节对齐。
    pub fn get_dirent64(&self, buf: &mut [u8]) -> Result<usize, isize> {
        if !self.is_dir() {
            return Err(crate::syscall::errno::ENOTDIR);
        }

        let dirents = self
            .inode
            .list_dirents()
            .map_err(|e| -(e as isize))?;
        let current_offset = self.offset.load(Ordering::SeqCst) as u64;
        let mut source_offset = 0u64;
        let mut new_offset = current_offset;
        let mut written = 0usize;

        for (name, ino, ft) in dirents {
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len().min(255);
            let raw_size = 8 + 8 + 2 + 1 + name_len + 1;
            let reclen = (raw_size + 7) & !7;
            let next_offset = source_offset + reclen as u64;

            if next_offset <= current_offset {
                source_offset = next_offset;
                continue;
            }
            if written + reclen > buf.len() {
                if written == 0 {
                    return Err(crate::syscall::errno::EINVAL);
                }
                break;
            }

            let pos = written;
            for b in &mut buf[pos..pos + reclen] {
                *b = 0;
            }

            let d_type = match ft {
                FileType::Dir => 4u8,
                FileType::File => 8u8,
                FileType::SymLink => 10u8,
                FileType::CharDevice => 2u8,
                FileType::BlockDevice => 6u8,
                FileType::Pipe => 5u8,
                FileType::Socket => 12u8,
                _ => 0u8,
            };

            buf[pos..pos + 8].copy_from_slice(&(ino as u64).to_le_bytes());
            buf[pos + 8..pos + 16].copy_from_slice(&(next_offset as i64).to_le_bytes());
            buf[pos + 16..pos + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
            buf[pos + 18] = d_type;
            buf[pos + 19..pos + 19 + name_len].copy_from_slice(&name_bytes[..name_len]);
            buf[pos + 19 + name_len] = 0;

            written += reclen;
            new_offset = next_offset;
            source_offset = next_offset;
        }

        self.offset.store(new_offset as usize, Ordering::SeqCst);
        Ok(written)
    }

    /// 挂起检测
    pub fn hang_up(&self) -> bool {
        false
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = self.inode.close(self.private_data.lock());
    }
}
