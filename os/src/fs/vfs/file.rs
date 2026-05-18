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
use core::fmt;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::{Mutex, MutexGuard};

use super::{FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata};
use crate::config::SYSTEM_FD_LIMIT;

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

    /// 创建空的 FdTable
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
            soft_limit: Self::INITIAL_CAPACITY,
            hard_limit: Self::MAX_CAPACITY,
        }
    }

    /// 克隆 FdTable（fork 时用）
    pub fn try_clone(&self) -> Result<Self, SyscallErr> {
        let mut fds = Vec::new();
        if fds.try_reserve(self.fds.len()).is_err() {
            return Err(SyscallErr::ENOMEM);
        }
        fds.extend(
            self.fds
                .iter()
                .map(|opt| opt.as_ref().and_then(|f| f.try_clone())),
        );

        let mut cloexec = Vec::new();
        if cloexec.try_reserve(self.cloexec.len()).is_err() {
            return Err(SyscallErr::ENOMEM);
        }
        cloexec.extend(self.cloexec.iter().copied());

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
        self.soft_limit = new_capacity;
        Ok(())
    }

    fn highest_open_index(&self) -> Option<usize> {
        self.fds.iter().rposition(|f| f.is_some())
    }

    // ── FD 分配/释放 ──────────────────────────────────────────────

    /// 分配一个新的文件描述符
    pub fn alloc_fd(&mut self, file: File, cloexec: bool) -> Result<usize, SyscallErr> {
        // 从 next_fd 开始扫描，找第一个空闲的
        let len = self.fds.len();
        for i in self.next_fd..len {
            if self.fds[i].is_none() {
                self.fds[i] = Some(file);
                self.cloexec[i] = cloexec;
                self.next_fd = i + 1;
                return Ok(i);
            }
        }

        // 从 0 到 next_fd 再扫一遍
        for i in 0..self.next_fd {
            if self.fds[i].is_none() {
                self.fds[i] = Some(file);
                self.cloexec[i] = cloexec;
                self.next_fd = i + 1;
                return Ok(i);
            }
        }

        // 没有空闲的，尝试扩容
        if len >= self.soft_limit {
            return Err(SyscallErr::EMFILE);
        }
        let new_capacity = core::cmp::min(len * 2, self.soft_limit);
        self.resize_to_capacity(new_capacity)?;
        // 递归分配
        self.alloc_fd(file, cloexec)
    }

    /// 在指定位置分配 fd（dup2 用）
    pub fn alloc_fd_at(
        &mut self,
        fd: usize,
        file: File,
        cloexec: bool,
    ) -> Result<usize, SyscallErr> {
        if fd >= self.soft_limit {
            return Err(SyscallErr::EBADF);
        }
        // 扩容到至少 fd + 1
        while self.fds.len() <= fd {
            let new_cap = core::cmp::min(self.fds.len() * 2, self.soft_limit);
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
    pub fn set_soft_limit(&mut self, limit: usize) { self.soft_limit = limit; }
    pub fn get_hard_limit(&self) -> usize { self.hard_limit }
    pub fn set_hard_limit(&mut self, limit: usize) { self.hard_limit = limit; }
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

        let n = self
            .inode
            .write_at(offset, len, buf, self.private_data.lock())?;

        if !flags.contains(FileFlags::O_APPEND) && n > 0 {
            self.offset.fetch_add(n, Ordering::SeqCst);
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
        self.inode
            .write_at(offset, buf.len(), buf, self.private_data.lock())
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
        match self.inode.poll(&*self.private_data.lock()) {
            Ok(revents) => (revents & super::event::EPollEvent::EPOLLIN.bits()) != 0,
            Err(_) => true, // ENOSYS fallback: 不支持 poll 的普通文件默认可读
        }
    }

    /// 写就绪检查（poll 用）
    pub fn w_ready(&self) -> bool {
        match self.inode.poll(&*self.private_data.lock()) {
            Ok(revents) => (revents & super::event::EPollEvent::EPOLLOUT.bits()) != 0,
            Err(_) => true,
        }
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

    // ── Clone ──────────────────────────────────────────────────────

    /// 尝试克隆 File（dup 时用）
    pub fn try_clone(&self) -> Option<Self> {
        let inode = self.inode.clone();
        let flags = *self.flags.lock();
        let private_data = Mutex::new(FilePrivateData::default());
        // 先检查 open 是否成功，避免失败后 Drop 错误调用 close
        if inode.open(private_data.lock(), &flags).is_err() {
            return None;
        }
        Some(File {
            inode,
            offset: Arc::clone(&self.offset),
            flags: Mutex::new(flags),
            mode: Mutex::new(*self.mode.lock()),
            file_type: self.file_type,
            private_data,
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
        let need_pages = if size == 0 { 0 } else { (size + PAGE_SIZE - 1) / PAGE_SIZE };

        // Helper: allocate frames + pread full file content into them
        let alloc_and_pread = |size: usize, need_pages: usize| -> Vec<Frame> {
            log::debug!(
                "[map_to_kernel_space] allocating {} frames + pread {} bytes",
                need_pages,
                size
            );
            let mut trackers: Vec<Arc<FrameTracker>> = Vec::with_capacity(need_pages);
            for _ in 0..need_pages {
                trackers.push(frame_alloc().expect("map_to_kernel_space: frame_alloc failed"));
            }
            let mut buf = alloc::vec![0u8; size];
            let n = self
                .pread(0, &mut buf)
                .expect("map_to_kernel_space: pread failed");
            if n != size {
                log::warn!(
                    "[map_to_kernel_space] pread returned {} bytes, expected {}",
                    n,
                    size
                );
            }
            let mut offset = 0;
            for tracker in &trackers {
                let dst = tracker.ppn.get_bytes_array();
                let chunk = (size - offset).min(PAGE_SIZE);
                dst[..chunk].copy_from_slice(&buf[offset..offset + chunk]);
                offset += chunk;
            }
            trackers.into_iter().map(Frame::InMemory).collect()
        };

        let frames: Vec<Frame> = if need_pages == 0 {
            Vec::new()
        } else if let Some(pc) = self.inode.page_cache() {
            let cached_frames = pc.frame_trackers();
            let cached_count = pc.cached_page_count();
            log::debug!(
                "[map_to_kernel_space] page_cache: {}/{} pages cached, {} frames tracked",
                cached_count,
                need_pages,
                cached_frames.len()
            );
            if cached_frames.len() >= need_pages {
                log::trace!("[map_to_kernel_space] using page_cache frames directly");
                cached_frames
            } else {
                // PageCache has insufficient frames — fall back to pread path.
                // This handles the case where a freshly created ext4 PageCache
                // has zero populated pages (lazy-init in get_new_page_cache).
                log::warn!(
                    "[map_to_kernel_space] page_cache only {}/{} frames, falling back to pread",
                    cached_frames.len(),
                    need_pages
                );
                alloc_and_pread(size, need_pages)
            }
        } else {
            log::trace!("[map_to_kernel_space] no page_cache, allocating from heap");
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

    // ── 桥接方法 (兼容旧 FileDescriptor API) ──────────────────────

    /// 读取用户缓冲区（桥接旧 syscall 接口）
    pub fn read_user(
        &self,
        offset: Option<usize>,
        buf: crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let count = buf.len();
        let mut kernel_buf = alloc::vec![0u8; count];
        let n = match offset {
            Some(off) => self.pread(off, &mut kernel_buf)?,
            None => self.read(&mut kernel_buf)?,
        };
        // copy into user
        let mut user_buf = buf;
        user_buf.write(&kernel_buf[..n]);
        Ok(n)
    }

    /// 写入用户缓冲区（桥接旧 syscall 接口）
    pub fn write_user(
        &self,
        offset: Option<usize>,
        buf: crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        let count = buf.len();
        let mut kernel_buf = alloc::vec![0u8; count];
        let user_buf = buf;
        user_buf.read(&mut kernel_buf);
        match offset {
            Some(off) => self.pwrite(off, &kernel_buf),
            None => self.write(&kernel_buf),
        }
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

    /// IOCTL（桥接旧接口）
    pub fn ioctl_old(&self, cmd: u32, arg: usize) -> isize {
        match self.inode.ioctl(cmd, arg, self.private_data.lock()) {
            Ok(n) => n as isize,
            Err(e) => -(e as isize),
        }
    }

    /// fcntl（桥接旧接口）
    pub fn fcntl_old(&self, _cmd: u32, _arg: u32) -> isize {
        // 基础实现留空，后续扩展
        0
    }

    /// 挂起检测
    pub fn hang_up(&self) -> bool {
        false
    }

    /// 获取当前工作目录的绝对路径（桥接旧 FileDescriptor::get_cwd）
    pub fn get_cwd(&self) -> Option<alloc::string::String> {
        self.inode.absolute_path().ok()
    }

    /// 切换工作目录（桥接旧 FileDescriptor::cd）
    pub fn cd(&self, path: &str) -> Result<Arc<Self>, isize> {
        use super::IndexNode as _;
        let start: Arc<dyn IndexNode> = self.inode.clone();
        let target = crate::fs::vfs_lookup(&start, path, true)?;
        let flags = self.flags();
        let new_file = File::new(target, flags).map_err(|e| -(e as isize))?;
        Ok(Arc::new(new_file))
    }

    /// 打开文件（桥接旧 FileDescriptor::open）
    pub fn open_path(
        &self,
        path: &str,
        flags: crate::fs::OpenFlags,
    ) -> Result<Self, isize> {
        use super::IndexNode as _;
        use crate::fs::vfs::FileType;

        if path.is_empty() {
            return Ok(self.try_clone().ok_or(crate::syscall::errno::ENOMEM)?);
        }
        if self.is_dir() == false && !path.starts_with('/') {
            return Err(crate::syscall::errno::ENOTDIR);
        }

        let start: Arc<dyn IndexNode> = self.inode.clone();
        let follow_final = !flags.contains(crate::fs::OpenFlags::O_NOFOLLOW);

        match crate::fs::vfs_lookup(&start, path, follow_final) {
            Ok(target) => {
                // 文件已存在
                if flags.contains(crate::fs::OpenFlags::O_CREAT | crate::fs::OpenFlags::O_EXCL) {
                    return Err(crate::syscall::errno::EEXIST);
                }

                let md = target.metadata().map_err(|e| e as isize)?;

                // 目录写权限检查
                if md.file_type == FileType::Dir
                    && (flags.contains(crate::fs::OpenFlags::O_WRONLY)
                        || flags.contains(crate::fs::OpenFlags::O_RDWR))
                {
                    return Err(crate::syscall::errno::EISDIR);
                }

                // O_DIRECTORY 检查
                if md.file_type != FileType::Dir
                    && flags.contains(crate::fs::OpenFlags::O_DIRECTORY)
                {
                    return Err(crate::syscall::errno::ENOTDIR);
                }

                // O_TRUNC
                if flags.contains(crate::fs::OpenFlags::O_TRUNC) {
                    target.resize(0).map_err(|e| e as isize)?;
                }

                let vfs_flags = flags_to_file_flags(flags);
                let file = File::new(target, vfs_flags).map_err(|e| -(e as isize))?;
                Ok(file)
            }
            Err(e) if e == crate::syscall::errno::ENOENT => {
                if !flags.contains(crate::fs::OpenFlags::O_CREAT)
                    || flags.contains(crate::fs::OpenFlags::O_DIRECTORY)
                {
                    return Err(e);
                }

                let (parent, leaf) = crate::fs::vfs_lookup_parent(path)?;
                let new_inode = parent
                    .create(&leaf, FileType::File, super::InodeMode::S_IRWXUGO)
                    .map_err(|err| err as isize)?;

                let vfs_flags = flags_to_file_flags(flags);
                let file = File::new(new_inode, vfs_flags).map_err(|err| -(err as isize))?;
                Ok(file)
            }
            Err(e) => Err(e),
        }
    }

    /// 创建目录（桥接旧 FileDescriptor::mkdir）
    pub fn mkdir_path(&self, path: &str) -> Result<(), isize> {
        use super::IndexNode as _;

        // 根路径 "/" 已存在，不创建
        if path == "/" || path == "." {
            return Err(crate::syscall::errno::EEXIST);
        }

        let start: Arc<dyn IndexNode> = self.inode.clone();
        let components = crate::fs::parse_path(path);
        let leaf = components.last().ok_or(crate::syscall::errno::ENOENT)?;
        let parent_path = if components.len() == 1 {
            if path.starts_with('/') { alloc::string::String::from("/") } else { alloc::string::String::from(".") }
        } else {
            let parent_comps = &components[..components.len() - 1];
            let joined = parent_comps.iter().map(|s| s.as_str()).collect::<alloc::vec::Vec<&str>>().join("/");
            if path.starts_with('/') { alloc::format!("/{}", joined) } else { joined }
        };

        let parent = crate::fs::vfs_lookup(&start, &parent_path, true)?;
        parent.mkdir(leaf, super::InodeMode::S_IRWXUGO).map_err(|e| e as isize)?;
        Ok(())
    }

    /// 删除文件或目录（桥接旧 FileDescriptor::delete）
    pub fn delete_path(&self, path: &str, delete_directory: bool) -> Result<(), isize> {
        use super::IndexNode as _;

        let (parent, leaf) = crate::fs::vfs_lookup_parent(path)?;
        if delete_directory {
            parent.rmdir(&leaf).map_err(|e| e as isize)
        } else {
            parent.unlink(&leaf).map_err(|e| e as isize)
        }
    }

    /// 获取 Stat（桥接旧 FileDescriptor::get_stat）
    pub fn get_stat_old(&self) -> crate::fs::layout::Stat {
        use crate::fs::layout::Stat;
        use crate::timer::TimeSpec;
        match self.metadata() {
            Ok(meta) => Stat {
                st_dev: meta.dev_id as u64,
                st_ino: meta.inode_id as u64,
                st_mode: meta.mode.bits() | crate::fs::vfs::InodeMode::from(meta.file_type).bits(),
                st_nlink: meta.nlinks as u32,
                st_uid: meta.uid,
                st_gid: meta.gid,
                st_rdev: meta.raw_dev as u64,
                __pad: 0,
                st_size: meta.size as i64,
                st_blksize: meta.blk_size as u32,
                __pad2: 0,
                st_blocks: meta.blocks as u64,
                st_atime: meta.atime,
                st_mtime: meta.mtime,
                st_ctime: meta.ctime,
                __unused: 0,
            },
            Err(_) => Stat {
                st_dev: 0,
                st_ino: 0,
                st_mode: 0,
                st_nlink: 0,
                st_uid: 0,
                st_gid: 0,
                st_rdev: 0,
                __pad: 0,
                st_size: 0,
                st_blksize: 0,
                __pad2: 0,
                st_blocks: 0,
                st_atime: TimeSpec::new(),
                st_mtime: TimeSpec::new(),
                st_ctime: TimeSpec::new(),
                __unused: 0,
            },
        }
    }

    /// 获取 Statx（桥接旧接口）
    pub fn get_statx_old(&self, mask: u32) -> crate::fs::layout::Statx {
        let stat = self.get_stat_old();
        crate::fs::layout::Statx::new(
            mask,
            stat.get_nlink(),
            stat.get_mode() as u16,
            stat.get_ino() as u64,
            stat.get_size() as u64,
            stat.get_atime() as i64,
            stat.get_ctime() as i64,
            stat.get_mtime() as i64,
            (stat.get_rdev() & 0xffff_00) >> 8 as u32,
            (stat.get_rdev() & 0xff) as u32,
            (stat.get_dev() & 0xffff_00) >> 8 as u32,
            (stat.get_dev() & 0xff) as u32,
        )
    }

    /// 设置时间戳（桥接旧 FileDescriptor::set_timestamp）
    pub fn set_timestamp_old(
        &self,
        _ctime: Option<usize>,
        _atime: Option<usize>,
        _mtime: Option<usize>,
    ) -> Result<(), isize> {
        // TODO: 通过 set_metadata 实现
        Ok(())
    }

    /// 打开子文件（旧接口兼容）
    pub fn open_subfile_old(
        &self,
    ) -> Result<Vec<(alloc::string::String, Arc<dyn IndexNode>)>, isize> {
        if !self.is_dir() {
            return Err(crate::syscall::errno::ENOTDIR);
        }
        let names = self
            .inode
            .list()
            .map_err(|_| crate::syscall::errno::ENOSYS)?;
        let mut subfiles = Vec::new();
        for name in names {
            match self.inode.find(&name) {
                Ok(inode) => subfiles.push((name, inode)),
                Err(_) => continue,
            }
        }
        Ok(subfiles)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = self.inode.close(self.private_data.lock());
    }
}

/// 将旧的 `OpenFlags` 转换为新的 `FileFlags`
fn flags_to_file_flags(flags: crate::fs::layout::OpenFlags) -> FileFlags {
    let mut result = FileFlags::empty();
    if flags.contains(crate::fs::layout::OpenFlags::O_RDONLY) {
        result |= FileFlags::O_RDONLY;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_WRONLY) {
        result |= FileFlags::O_WRONLY;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_RDWR) {
        result |= FileFlags::O_RDWR;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_CREAT) {
        result |= FileFlags::O_CREAT;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_EXCL) {
        result |= FileFlags::O_EXCL;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_TRUNC) {
        result |= FileFlags::O_TRUNC;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_APPEND) {
        result |= FileFlags::O_APPEND;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_NONBLOCK) {
        result |= FileFlags::O_NONBLOCK;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_DIRECTORY) {
        result |= FileFlags::O_DIRECTORY;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_CLOEXEC) {
        result |= FileFlags::O_CLOEXEC;
    }
    if flags.contains(crate::fs::layout::OpenFlags::O_NOFOLLOW) {
        result |= FileFlags::O_NOFOLLOW;
    }
    result
}
