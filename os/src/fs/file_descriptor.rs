use super::{
    dirent::Dirent, directory_tree, vfs, vfs_lookup, vfs_lookup_absolute, vfs_lookup_parent,
    Statx,
};
use crate::{
    config::SYSTEM_FD_LIMIT,
    mm::{Frame, UserBuffer},
    syscall::errno::*,
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::convert::TryInto;
use core::slice::{Iter, IterMut};
use spin::Mutex;

use super::layout::{OpenFlags, SeekWhence, Stat};
use super::vfs::InodeMode;

/// 将旧 OpenFlags 转换为新 VFS FileFlags（pub 供 vfs::FdTable 桥接使用）
pub fn _open_flags_to_vfs_flags(o: OpenFlags) -> vfs::FileFlags {
    let mut f = vfs::FileFlags::empty();
    if o.contains(OpenFlags::O_RDONLY) { f.insert(vfs::FileFlags::O_RDONLY); }
    if o.contains(OpenFlags::O_WRONLY) { f.insert(vfs::FileFlags::O_WRONLY); }
    if o.contains(OpenFlags::O_RDWR) { f.insert(vfs::FileFlags::O_RDWR); }
    if o.contains(OpenFlags::O_CREAT) { f.insert(vfs::FileFlags::O_CREAT); }
    if o.contains(OpenFlags::O_TRUNC) { f.insert(vfs::FileFlags::O_TRUNC); }
    if o.contains(OpenFlags::O_APPEND) { f.insert(vfs::FileFlags::O_APPEND); }
    if o.contains(OpenFlags::O_NONBLOCK) { f.insert(vfs::FileFlags::O_NONBLOCK); }
    if o.contains(OpenFlags::O_DIRECTORY) { f.insert(vfs::FileFlags::O_DIRECTORY); }
    if o.contains(OpenFlags::O_CLOEXEC) { f.insert(vfs::FileFlags::O_CLOEXEC); }
    if o.contains(OpenFlags::O_NOFOLLOW) { f.insert(vfs::FileFlags::O_NOFOLLOW); }
    if o.contains(OpenFlags::O_PATH) { f.insert(vfs::FileFlags::O_PATH); }
    f
}

#[derive(Clone)]
pub struct FileDescriptor {
    cloexec: bool,
    nonblock: bool,
    flags: OpenFlags,
    /// 新 VFS inode（替代旧 Arc<dyn File>）
    pub file: Arc<dyn vfs::IndexNode>,
    /// 文件私有数据（用于 IndexNode::read_at/write_at 等）
    private_data: Arc<Mutex<vfs::FilePrivateData>>,
}

#[allow(unused)]
impl FileDescriptor {
    pub fn new(cloexec: bool, nonblock: bool, file: Arc<dyn vfs::IndexNode>) -> Self {
        Self {
            cloexec,
            nonblock,
            flags: OpenFlags::empty(),
            file,
            private_data: Arc::new(Mutex::new(vfs::FilePrivateData::default())),
        }
    }

    pub fn get_flags(&self) -> OpenFlags {
        self.flags
    }
    pub fn set_flags(&mut self, flags: OpenFlags) {
        self.flags = flags;
    }
    pub fn set_cloexec(&mut self, flag: bool) {
        self.cloexec = flag;
    }
    pub fn get_cloexec(&self) -> bool {
        self.cloexec
    }
    pub fn get_nonblock(&self) -> bool {
        self.nonblock
    }
    pub fn set_nonblock(&mut self, flag: bool) {
        self.nonblock = flag;
    }

    fn is_dir_inode(&self) -> bool {
        self.file.metadata()
            .map(|m| m.file_type == vfs::FileType::Dir)
            .unwrap_or(false)
    }

    pub fn get_cwd(&self) -> Option<String> {
        self.file.absolute_path().ok()
    }

    /// Just used for cwd
    pub fn cd(&self, path: &str) -> Result<Arc<Self>, isize> {
        match self.open(path, OpenFlags::O_DIRECTORY | OpenFlags::O_RDONLY, true) {
            Ok(fd) => Ok(Arc::new(fd)),
            Err(errno) => Err(errno),
        }
    }

    pub fn readable(&self) -> bool {
        self.file.metadata().is_ok()
    }
    pub fn writable(&self) -> bool {
        self.file.metadata().is_ok()
    }

    pub fn read(&self, offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
        let off = offset.as_deref().copied().unwrap_or(0);
        let len = buf.len();
        let Ok(n) = self.file.read_at(off, len, buf, self.private_data.lock()) else {
            return 0;
        };
        if let Some(o) = offset {
            *o += n;
        }
        n
    }

    pub fn write(&self, offset: Option<&mut usize>, buf: &[u8]) -> usize {
        let off = offset.as_deref().copied().unwrap_or(0);
        let len = buf.len();
        let Ok(n) = self.file.write_at(off, len, buf, self.private_data.lock()) else {
            return 0;
        };
        if let Some(o) = offset {
            *o += n;
        }
        n
    }

    pub fn r_ready(&self) -> bool {
        self.file
            .poll(&*self.private_data.lock())
            .map(|v| v & (1 << 0) != 0) // POLLIN
            .unwrap_or(true)
    }
    pub fn w_ready(&self) -> bool {
        self.file
            .poll(&*self.private_data.lock())
            .map(|v| v & (1 << 2) != 0) // POLLOUT
            .unwrap_or(true)
    }
    pub fn hang_up(&self) -> bool {
        false
    }

    pub fn read_user(&self, offset: Option<usize>, mut buf: UserBuffer) -> usize {
        let off = offset.unwrap_or(0);
        let mut kernel_buf = alloc::vec![0u8; buf.len()];
        let n = self
            .file
            .read_at(off, kernel_buf.len(), &mut kernel_buf, self.private_data.lock())
            .unwrap_or(0);
        buf.write_at(0, &kernel_buf[..n]);
        n
    }

    pub fn write_user(&self, offset: Option<usize>, buf: UserBuffer) -> usize {
        let off = offset.unwrap_or(0);
        let mut kernel_buf = alloc::vec![0u8; buf.len()];
        buf.read_at(0, &mut kernel_buf);
        self.file
            .write_at(off, kernel_buf.len(), &kernel_buf, self.private_data.lock())
            .unwrap_or(0)
    }

    pub fn get_stat(&self) -> Stat {
        if let Ok(md) = self.file.metadata() {
            let raw_mode = md.mode.bits() | InodeMode::from(md.file_type).bits();
            Stat::new(
                md.dev_id as u64,
                md.inode_id as u64,
                raw_mode,
                md.nlinks as u32,
                md.raw_dev,
                md.size,
                md.atime.tv_sec as i64,
                md.mtime.tv_sec as i64,
                md.ctime.tv_sec as i64,
            )
        } else {
            Stat::new(0, 0, 0, 0, 0, 0, 0, 0, 0)
        }
    }

    pub fn get_statx(&self, mask: u32) -> Statx {
        let stat = self.get_stat();
        Statx::new(
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

    pub fn open(&self, path: &str, flags: OpenFlags, _special_use: bool) -> Result<Self, isize> {
        if path == "" {
            return Ok(self.clone());
        }
        let vfs_flags = _open_flags_to_vfs_flags(flags);
        let root: Arc<dyn vfs::IndexNode> = super::vfs_root().mountpoint_root_inode();

        let inode = if path.starts_with('/') {
            // 绝对路径
            vfs_lookup(&root, path, !flags.contains(OpenFlags::O_NOFOLLOW))?
        } else {
            // 相对路径：从当前 inode（应是一个目录）开始解析
            vfs_lookup(&self.file, path, !flags.contains(OpenFlags::O_NOFOLLOW))?
        };

        if flags.contains(OpenFlags::O_DIRECTORY) {
            let md = inode.metadata().map_err(|_| ENOTDIR)?;
            if md.file_type != vfs::FileType::Dir {
                return Err(ENOTDIR);
            }
        }

        let cloexec = flags.contains(OpenFlags::O_CLOEXEC);
        let mut fd = Self::new(cloexec, false, inode);
        fd.flags = flags;
        Ok(fd)
    }

    pub fn mkdir(&self, path: &str) -> Result<(), isize> {
        let (parent, name) = vfs_lookup_parent(path)?;
        parent.create(&name, vfs::FileType::Dir, InodeMode::S_IRWXUGO)
            .map(|_| ())
            .map_err(|e| e as isize)
    }

    pub fn delete(&self, path: &str, delete_directory: bool) -> Result<(), isize> {
        let (parent, name) = vfs_lookup_parent(path)?;
        if delete_directory {
            parent.rmdir(&name).map_err(|e| e as isize)
        } else {
            parent.unlink(&name).map_err(|e| e as isize)
        }
    }

    pub fn rename(
        old_fd: &Self,
        old_path: &str,
        new_fd: &Self,
        new_path: &str,
    ) -> Result<(), isize> {
        // 使用 DirectoryTreeNode 的 rename（暂时保留此桥接）
        let old_abs = old_fd.get_cwd().map(|d| alloc::format!("{}/{}", d, old_path))
            .ok_or(ENOENT)?;
        let new_abs = new_fd.get_cwd().map(|d| alloc::format!("{}/{}", d, new_path))
            .ok_or(ENOENT)?;
        directory_tree::DirectoryTreeNode::rename(&old_abs, &new_abs)
    }

    /// 获取目录项数组
    pub fn get_dirent(&self, count: usize) -> Result<Vec<Dirent>, isize> {
        let md = self.file.metadata().map_err(|_| ENOTDIR)?;
        if md.file_type != vfs::FileType::Dir {
            return Err(ENOTDIR);
        }
        let entries = self.file.list().map_err(|_| ENOTDIR)?;
        let mut result = Vec::new();
        for name in entries.iter().take(count) {
            if let Ok(child) = self.file.find(name) {
                if let Ok(cmd) = child.metadata() {
                    result.push(Dirent {
                        d_ino: cmd.inode_id as usize,
                        d_off: 0,
                        d_reclen: name.len() as u16,
                        d_type: 0,
                        d_name: [0u8; 128],
                    });
                }
            }
        }
        Ok(result)
    }

    pub fn get_offset(&self) -> usize {
        self.lseek(0, SeekWhence::SEEK_CUR).unwrap_or(0)
    }

    pub fn lseek(&self, offset: isize, whence: SeekWhence) -> Result<usize, isize> {
        // lseek 不再有 inode 级别的方法，改为在 FileDescriptor 层简单跟踪
        // FIXME: 需要使用 vfs::File 来跟踪 offset，但 FileDescriptor 不跟踪 offset
        Err(EINVAL)
    }

    pub fn get_size(&self) -> usize {
        self.file.metadata()
            .map(|m| m.size as usize)
            .unwrap_or(0)
    }

    pub fn modify_size(&self, diff: isize) -> Result<(), isize> {
        let cur = self.get_size() as isize;
        let new = cur + diff;
        if new < 0 { return Err(EINVAL); }
        self.file.resize(new as usize).map_err(|e| e as isize)
    }

    pub fn truncate_size(&self, new_size: isize) -> Result<(), isize> {
        if new_size < 0 {
            return Err(EINVAL);
        }
        self.file.resize(new_size as usize).map_err(|e| e as isize)
    }

    pub fn set_timestamp(
        &self,
        _ctime: Option<usize>,
        _atime: Option<usize>,
        _mtime: Option<usize>,
    ) -> Result<(), isize> {
        // IndexNode 的 metadata 是只读的，暂时忽略时间戳设置
        Ok(())
    }

    pub fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<super::cache::PageCache>>, ()> {
        // FIXME: 桥接方法在新架构中不再使用，调用方需要迁移到新 PageCache
        Err(())
    }

    /// 获取文件的所有缓存
    pub fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<super::cache::PageCache>>>, ()> {
        // FIXME: 桥接方法在新架构中不再使用
        Err(())
    }

    pub fn ioctl(&self, cmd: u32, argp: usize) -> isize {
        self.file.ioctl(cmd, argp, self.private_data.lock())
            .map(|n| n as isize)
            .unwrap_or_else(|e| -(e as isize))
    }

    /// 映射到内核空间
    pub fn map_to_kernel_space(&self, addr: usize) -> &'static [u8] {
        let frames: Vec<Frame> = self
            .file
            .page_cache()
            .map(|pc| pc.frame_trackers())
            .unwrap_or_default();

        crate::mm::KERNEL_SPACE
            .lock()
            .insert_program_area(
                crate::mm::VirtAddr::from(addr).try_into().unwrap(),
                crate::mm::MapPermission::R | crate::mm::MapPermission::W,
                frames,
            )
            .unwrap();

        let size = self.get_size();
        unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, size) }
    }
}

/// ### 文件描述符表（旧版，syscall 迁移期间保留）
#[derive(Clone)]
pub struct FdTable {
    // 文件描述符 数组
    inner: Vec<Option<FileDescriptor>>,
    // 已回收的文件描述符
    recycled: Vec<u8>,
    soft_limit: usize,
    hard_limit: usize,
}

/// ### 文件描述符表的方法
#[allow(unused)]
impl FdTable {
    pub const DEFAULT_FD_LIMIT: usize = 128;
    pub const SYSTEM_FD_LIMIT: usize = SYSTEM_FD_LIMIT;
    pub fn new(inner: Vec<Option<FileDescriptor>>) -> Self {
        Self {
            inner,
            recycled: Vec::new(),
            soft_limit: FdTable::DEFAULT_FD_LIMIT,
            hard_limit: FdTable::SYSTEM_FD_LIMIT,
        }
    }
    pub fn try_clone(&self) -> Result<Self, isize> {
        let mut inner: Vec<Option<FileDescriptor>> = Vec::new();
        if inner.try_reserve(self.inner.len()).is_err() {
            return Err(ENOMEM);
        }
        inner.extend(self.inner.iter().cloned());

        let mut recycled: Vec<u8> = Vec::new();
        if recycled.try_reserve(self.recycled.len()).is_err() {
            return Err(ENOMEM);
        }
        recycled.extend(self.recycled.iter().copied());

        Ok(Self {
            inner,
            recycled,
            soft_limit: self.soft_limit,
            hard_limit: self.hard_limit,
        })
    }
    pub fn get_soft_limit(&self) -> usize {
        self.soft_limit
    }
    pub fn set_soft_limit(&mut self, limit: usize) {
        if limit < self.inner.len() {
            log::warn!(
                "[FdTable::set_soft_limit] new limit: {} is smaller than current table length: {}",
                self.inner.len(),
                self.soft_limit
            );
            self.inner.truncate(limit);
            self.recycled.retain(|&fd| (fd as usize) < limit);
        }
        self.soft_limit = limit;
    }
    pub fn get_hard_limit(&self) -> usize {
        self.hard_limit
    }
    pub fn set_hard_limit(&mut self, limit: usize) {
        if limit < self.inner.len() {
            log::warn!(
                "[FdTable::set_hard_limit] new limit: {} is smaller than current table length: {}",
                self.inner.len(),
                self.soft_limit
            );
            self.inner.truncate(limit);
            self.recycled.retain(|&fd| (fd as usize) < limit);
        }
        self.hard_limit = limit;
    }
    #[inline]
    pub fn get_ref(&self, fd: usize) -> Result<&FileDescriptor, isize> {
        if fd >= self.inner.len() {
            return Err(EBADF);
        }
        match &self.inner[fd] {
            Some(file_descriptor) => Ok(file_descriptor),
            None => Err(EBADF),
        }
    }
    #[inline]
    pub fn get_refmut(&mut self, fd: usize) -> Result<&mut FileDescriptor, isize> {
        if fd >= self.inner.len() {
            return Err(EBADF);
        }
        match &mut self.inner[fd] {
            Some(file_descriptor) => Ok(file_descriptor),
            None => Err(EBADF),
        }
    }
    #[inline]
    pub fn remove(&mut self, fd: usize) -> Result<FileDescriptor, isize> {
        if fd >= self.inner.len() {
            return Err(EBADF);
        }
        match self.inner[fd].take() {
            Some(file_descriptor) => {
                self.recycled.push(fd as u8);
                Ok(file_descriptor)
            }
            None => Err(EBADF),
        }
    }
    #[inline(always)]
    pub fn iter(&self) -> Iter<Option<FileDescriptor>> {
        self.inner.iter()
    }
    #[inline(always)]
    pub fn iter_mut(&mut self) -> IterMut<Option<FileDescriptor>> {
        self.inner.iter_mut()
    }
    /// check if `fd` is valid
    #[inline]
    pub fn check(&self, fd: usize) -> Result<(), isize> {
        if fd >= self.inner.len() || self.inner[fd].is_none() {
            return Err(EBADF);
        }
        Ok(())
    }
    pub fn find_min(&mut self) -> Option<u8> {
        if let Some(&min_value) = self.recycled.iter().min() {
            if let Some(index) = self.recycled.iter().position(|&x| x == min_value) {
                self.recycled.remove(index);
                Some(min_value)
            } else {
                None
            }
        } else {
            None
        }
    }
    #[inline]
    pub fn insert(&mut self, file_descriptor: FileDescriptor) -> Result<usize, isize> {
        let fd = match self.find_min() {
            Some(fd) => {
                self.inner[fd as usize] = Some(file_descriptor);
                fd as usize
            }
            None => {
                let current = self.inner.len();
                if current == self.soft_limit {
                    return Err(EMFILE);
                } else {
                    self.inner.push(Some(file_descriptor));
                    current
                }
            }
        };
        Ok(fd)
    }

    /// insert at `pos`, if there is an existing fd, it will be replaced.
    #[inline]
    pub fn insert_at(
        &mut self,
        file_descriptor: FileDescriptor,
        pos: usize,
    ) -> Result<usize, isize> {
        let current = self.inner.len();
        if pos < current {
            if self.inner[pos].is_none() {
                self.recycled.retain(|&fd| fd as usize != pos);
            }
            self.inner[pos] = Some(file_descriptor);
            Ok(pos)
        } else {
            if pos >= self.soft_limit {
                return Err(EMFILE);
            } else {
                (current..pos)
                    .rev()
                    .for_each(|fd| self.recycled.push(fd as u8));
                self.inner.resize(pos, None);
                self.inner.push(Some(file_descriptor));
                Ok(pos)
            }
        }
    }

    /// try to insert at the lowest-numbered available fd greater than or equal to `hint`(no replace)
    #[inline]
    pub fn try_insert_at(
        &mut self,
        file_descriptor: FileDescriptor,
        hint: usize,
    ) -> Result<usize, isize> {
        if hint >= self.soft_limit {
            return Err(EMFILE);
        }
        let current = self.inner.len();
        if hint < current {
            match self.inner[hint] {
                Some(_) => match self.recycled.iter().copied().find(|&fd| fd as usize > hint) {
                    Some(fd) => {
                        self.inner[fd as usize] = Some(file_descriptor);
                        Ok(fd as usize)
                    }
                    None => {
                        if current == self.soft_limit {
                            return Err(EMFILE);
                        } else {
                            self.inner.push(Some(file_descriptor));
                            Ok(current)
                        }
                    }
                },
                None => {
                    self.recycled.retain(|&fd| fd as usize != hint);
                    self.inner[hint] = Some(file_descriptor);
                    Ok(hint)
                }
            }
        } else {
            if hint >= self.soft_limit {
                return Err(EMFILE);
            } else {
                (current..hint).for_each(|fd| self.recycled.push(fd as u8));
                self.inner.resize(hint, None);
                self.inner.push(Some(file_descriptor));
                Ok(hint)
            }
        }
    }
    /// Take the ownership of the given fd
    pub fn take(&mut self, fd: usize) -> Option<FileDescriptor> {
        if fd >= self.inner.len() {
            None
        } else {
            self.inner[fd].take()
        }
    }
}
