//! MountFS — VFS 挂载层
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/mount.rs` 的 `MountFS` / `MountFSInode`。
//!
//! 设计思想：
//! - `MountFS` 包装一个 `Arc<dyn FileSystem>`，同时维护子挂载点表
//! - `MountFSInode` 包装一个 `Arc<dyn IndexNode>`，实现 `IndexNode` trait，
//!   所有操作委托给 `inner_inode`，在路径解析时跨越挂载点边界
//! - 全局 `MountList` 管理所有挂载关系（路径 → 挂载点映射）
//!
//! 路径解析流程示例（"/mnt/ext4/file"）：
//!   根 MountFSInode.find("mnt")
//!     → inner_inode.find("mnt") → 返回 mnt inode
//!     → 检查 mountpoints 表：mnt 是挂载点 → 返回 ext4 的根 MountFSInode
//!       → ext4根.find("file") → 返回目标 inode

use crate::utils::error::SyscallErr;
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt::Debug;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use spin::{Mutex, MutexGuard};
use lazy_static::lazy_static;

use super::dentry_cache::DentryCache;
use super::{
    file::FileFlags, file_system::FileSystem, propagation::{MountPropagation, PropagationType, register_peer, propagate_mount, propagate_umount, unregister_peer_mount},
    FilePrivateData, FileType, IndexNode, InodeId, InodeMode,
};

// ── MountFlags ──────────────────────────────────────────────────────────

bitflags! {
    /// 挂载标志，对标 Linux mount.h
    pub struct MountFlags: u32 {
        /// 只读挂载
        const RDONLY = 0x1;
        /// 忽略 suid/sgid
        const NOSUID = 0x2;
        /// 禁止设备特殊文件
        const NODEV = 0x4;
        /// 禁止执行
        const NOEXEC = 0x8;
        /// 同步写入
        const SYNCHRONOUS = 0x10;
        /// 重新挂载
        const REMOUNT = 0x20;
        /// 允许强制锁
        const MANDLOCK = 0x40;
        /// 目录修改同步
        const DIRSYNC = 0x80;
        /// 不更新访问时间
        const NOATIME = 0x400;
        /// 不更新目录访问时间
        const NODIRATIME = 0x800;
        /// bind mount
        const BIND = 0x1000;
        /// 重新递归 bind mount
        const REC = 0x4000;
    }
}

// ── MountPath ────────────────────────────────────────────────────────────

/// 挂载路径，用于全局挂载表
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MountPath(pub String);

impl From<&str> for MountPath {
    fn from(value: &str) -> Self {
        MountPath(String::from(value))
    }
}

impl From<String> for MountPath {
    fn from(value: String) -> Self {
        MountPath(value)
    }
}

impl AsRef<str> for MountPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Ord for MountPath {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for MountPath {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

// ── MountFSInode ────────────────────────────────────────────────────────

/// Debug: lifetime counters for MountFS / MountFSInode
pub mod counters {
    use core::sync::atomic::AtomicUsize;
    pub static MOUNTFS_ALIVE: AtomicUsize = AtomicUsize::new(0);
    pub static MOUNTFSINODE_ALIVE: AtomicUsize = AtomicUsize::new(0);

    pub fn mountfs_alive() -> usize { MOUNTFS_ALIVE.load(core::sync::atomic::Ordering::Relaxed) }
    pub fn mountfsinode_alive() -> usize { MOUNTFSINODE_ALIVE.load(core::sync::atomic::Ordering::Relaxed) }
}

/// MountFSInode — 挂载感知的 inode 包装器
///
/// 包装内层 inode，所有 `IndexNode` 方法委托给 `inner_inode`。
/// 在 `find()` 中检查子挂载点表，实现跨文件系统路径解析。
#[derive(Debug)]
pub struct MountFSInode {
    /// 内层 inode
    pub inner_inode: Arc<dyn IndexNode>,
    /// 所属的 MountFS
    pub mount_fs: Arc<MountFS>,
    /// 指向自身的弱引用
    self_ref: Mutex<Weak<MountFSInode>>,
}

impl MountFSInode {
    /// 创建新 MountFSInode
    pub fn new(inner_inode: Arc<dyn IndexNode>, mount_fs: Arc<MountFS>) -> Arc<Self> {
        counters::MOUNTFSINODE_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Arc::new_cyclic(|self_ref| MountFSInode {
            inner_inode,
            mount_fs,
            self_ref: Mutex::new(self_ref.clone()),
        })
    }

    /// 获取自身的强引用
    fn self_arc(&self) -> Arc<Self> {
        self.self_ref.lock().upgrade().unwrap()
    }

    /// 如果是 MountFSInode 包装，解包到内层 inode；否则原样返回
    pub fn unwrap_inode(inode: &Arc<dyn IndexNode>) -> Arc<dyn IndexNode> {
        if let Some(mnt) = inode.as_any_ref().downcast_ref::<MountFSInode>() {
            mnt.inner_inode.clone()
        } else {
            inode.clone()
        }
    }

    /// 检查挂载是否可写
    fn ensure_mount_writable(&self) -> Result<(), SyscallErr> {
        if self.mount_fs.mount_flags().contains(MountFlags::RDONLY) {
            return Err(SyscallErr::EROFS);
        }
        Ok(())
    }

    /// 判断当前 inode 是否为挂载点根
    pub fn is_mountpoint_root(&self) -> bool {
        let Ok(cur_md) = self.inner_inode.metadata() else { return false };
        let root_inner = self.mount_fs.root_inner_inode();
        let Ok(root_md) = root_inner.metadata() else { return false };
        cur_md.inode_id == root_md.inode_id
    }

    /// 解析路径时，跨越挂载点边界
    ///
    /// 如果在当前 inode 的子挂载表中找到了匹配的 inode_id，
    /// 返回子文件系统的根 inode。
    fn overlaid_inode(self_inode: Arc<MountFSInode>) -> Arc<MountFSInode> {
        let inode_id = match self_inode.inner_inode.metadata() {
            Ok(md) => md.inode_id,
            Err(_) => return self_inode,
        };
        let sub_mountfs = {
            let lock = self_inode.mount_fs.mountpoints.lock();
            lock.get(&inode_id).cloned()
        };
        match sub_mountfs {
            Some(sub) => sub.mountpoint_root_inode(),
            None => self_inode,
        }
    }

    /// 逐级查找子项（带挂载点交叉和 dentry 缓存）
    fn do_find(&self, name: &str) -> Result<Arc<MountFSInode>, SyscallErr> {
        // Shortcut: skip dentry cache for dynamic filesystems (procfs)
        if self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            let inner_inode = self.inner_inode.find(name)?;
            return Ok(MountFSInode::overlaid_inode(MountFSInode::new(
                inner_inode,
                self.mount_fs.clone(),
            )));
        }

        let parent_ino = self.inner_inode.metadata()?.inode_id;
        let key = super::dentry_cache::DentryKey {
            parent_ino,
            name: String::from(name),
        };

        // Check dentry cache — returns covered dentry
        if let Some(cached) = self.mount_fs.dentry_cache.lock().get(&key) {
            return Ok(MountFSInode::overlaid_inode(cached));
        }

        // Cache miss: record generation before disk I/O
        let gen_before = self.mount_fs.dentry_gen.load(core::sync::atomic::Ordering::Acquire);

        // Release cache lock, perform actual filesystem lookup
        let inner_inode = self.inner_inode.find(name)?;

        // Create covered dentry (before mount-point overlay)
        let covered = MountFSInode::new(inner_inode, self.mount_fs.clone());

        // Insert into cache — only if directory was not modified concurrently
        let gen_after = self.mount_fs.dentry_gen.load(core::sync::atomic::Ordering::Acquire);
        if gen_before == gen_after {
            let (entry, evicted) = self.mount_fs.dentry_cache.lock().insert_or_get(key, covered);
            drop(evicted); // Drop outside lock
            Ok(MountFSInode::overlaid_inode(entry))
        } else {
            // Directory was modified (unlink/rename/etc.), don't cache stale dentry
            Ok(MountFSInode::overlaid_inode(covered))
        }
    }

    /// 查找父目录
    fn do_parent(&self) -> Result<Arc<MountFSInode>, SyscallErr> {
        if self.is_mountpoint_root() {
            // 如果当前是挂载点根，父目录在其父文件系统的挂载点
            if let Some(mountpoint) = self.mount_fs.self_mountpoint() {
                return Ok(mountpoint);
            }
            // 没有挂载点，返回自己（全局根）
            return Ok(self.self_arc());
        }
        // 向 inner_inode 请求父目录
        let parent_inner = self.inner_inode.find("..")?;
        Ok(MountFSInode::new(parent_inner, self.mount_fs.clone()))
    }

    /// Create a new MountFS rooted at `root_inner_inode` and attach it as a
    /// child of this MountFSInode's parent MountFS at this inode's position.
    pub fn mount_subtree(
        &self,
        inner_fs: Arc<dyn FileSystem>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
        mount_path: Option<String>,
    ) -> Result<Arc<MountFS>, SyscallErr> {
        let metadata = self.inner_inode.metadata()?;
        if metadata.file_type != FileType::Dir {
            return Err(SyscallErr::ENOTDIR);
        }
        let inode_id = metadata.inode_id;

        let new_mount_fs = MountFS::new_with_root(inner_fs, root_inner_inode, mount_flags);

        // Inherit parent's propagation type if parent is shared
        let parent_prop = self.mount_fs.propagation();
        if parent_prop.is_shared() {
            let gid = parent_prop.peer_group_id();
            new_mount_fs.propagation().set_shared_with_group(gid);
            register_peer(&new_mount_fs);
        }

        let backref = MountFSInode::new(self.inner_inode.clone(), self.mount_fs.clone());
        new_mount_fs.set_self_mountpoint(Some(backref));

        self.mount_fs.add_mount(inode_id, new_mount_fs.clone())?;

        new_mount_fs.set_mount_path(mount_path);

        // Register in global mount list
        if let Some(ref path) = new_mount_fs.mount_path() {
            MOUNT_LIST.insert(path.as_str(), new_mount_fs.clone(), Some(inode_id));
        }

        // Propagate to peers if parent is shared
        if parent_prop.is_shared() {
            // Peer group membership alone provides visibility across peers;
            // avoid creating duplicate MountFS instances that break umount cleanup.
        }

        Ok(new_mount_fs)
    }
}

impl IndexNode for MountFSInode {
    fn read_at(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.read_at(offset, len, buf, data)
    }

    fn write_at(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_at(offset, len, buf, data)
    }

    fn read_direct(
        &self,
        offset: usize,
        len: usize,
        buf: &mut [u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.read_direct(offset, len, buf, data)
    }

    fn write_direct(
        &self,
        offset: usize,
        len: usize,
        buf: &[u8],
        data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_direct(offset, len, buf, data)
    }

    fn read_sync(&self, offset: usize, buf: &mut [u8]) -> Result<usize, SyscallErr> {
        self.inner_inode.read_sync(offset, buf)
    }

    fn write_sync(&self, offset: usize, buf: &[u8]) -> Result<usize, SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.write_sync(offset, buf)
    }

    fn open(&self, data: MutexGuard<FilePrivateData>, flags: &FileFlags) -> Result<(), SyscallErr> {
        self.inner_inode.open(data, flags)
    }

    fn close(&self, data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        self.inner_inode.close(data)
    }

    fn find(&self, name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.do_find(name)
            .map(|mnt_inode| mnt_inode as Arc<dyn IndexNode>)
    }

    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        self.inner_inode.list()
    }

    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        self.inner_inode.list_dirents()
    }

    fn create(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);
        let inner_inode = self.inner_inode.create(name, file_type, mode)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = self.mount_fs.dentry_cache.lock()
                    .insert_or_get(key, wrapper.clone());
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn symlink(&self, name: &str, target: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);
        let inner_inode = self.inner_inode.symlink(name, target)?;
        let wrapper = MountFSInode::new(inner_inode, self.mount_fs.clone());
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let (_, evicted) = self.mount_fs.dentry_cache.lock()
                    .insert_or_get(key, wrapper.clone());
                drop(evicted);
            }
        }
        Ok(wrapper)
    }

    fn link(&self, name: &str, other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);
        let other = MountFSInode::unwrap_inode(other);
        self.inner_inode.link(name, &other)?;
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let linked = MountFSInode::new(
                    self.inner_inode.find(name).unwrap_or(other),
                    self.mount_fs.clone(),
                );
                let (_, evicted) = self.mount_fs.dentry_cache.lock()
                    .insert_or_get(key, linked);
                drop(evicted);
            }
        }
        Ok(())
    }

    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
    ) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);

        let new_parent = MountFSInode::unwrap_inode(new_parent);
        self.inner_inode.rename(old_name, &new_parent, new_name)?;

        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let old_key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(old_name),
                };
                let old_evicted = self.mount_fs.dentry_cache.lock().invalidate(&old_key);
                drop(old_evicted);
            }
        }
        Ok(())
    }

    fn unlink(&self, name: &str) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);
        // 检查是否为挂载点
        if let Ok(inode) = self.inner_inode.find(name) {
            let inode_id = inode.metadata()?.inode_id;
            if self.mount_fs.mountpoints.lock().contains_key(&inode_id) {
                return Err(SyscallErr::EBUSY);
            }
        }
        self.inner_inode.unlink(name)?;
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let removed = self.mount_fs.dentry_cache.lock().invalidate(&key);
                drop(removed);
            }
        }
        Ok(())
    }

    fn rmdir(&self, name: &str) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.mount_fs.dentry_gen.fetch_add(1, core::sync::atomic::Ordering::Release);
        // 检查是否为挂载点
        let child_inode_id = if let Ok(inode) = self.inner_inode.find(name) {
            let inode_id = inode.metadata()?.inode_id;
            if self.mount_fs.mountpoints.lock().contains_key(&inode_id) {
                return Err(SyscallErr::EBUSY);
            }
            Some(inode_id)
        } else {
            None
        };
        self.inner_inode.rmdir(name)?;
        if !self.mount_fs.no_dentry_cache.load(Ordering::Relaxed) {
            if let Ok(parent_md) = self.inner_inode.metadata() {
                let key = super::dentry_cache::DentryKey {
                    parent_ino: parent_md.inode_id,
                    name: String::from(name),
                };
                let removed = self.mount_fs.dentry_cache.lock().invalidate(&key);
                drop(removed);
            }
            if let Some(child_ino) = child_inode_id {
                let evicted = self.mount_fs.dentry_cache.lock().clear_parent(child_ino);
                drop(evicted);
            }
        }
        Ok(())
    }

    fn metadata(&self) -> Result<super::Metadata, SyscallErr> {
        self.inner_inode.metadata()
    }

    fn set_metadata(&self, metadata: &super::Metadata) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.set_metadata(metadata)
    }

    fn resize(&self, len: usize) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.resize(len)
    }

    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        self.ensure_mount_writable()?;
        self.inner_inode.truncate(len)
    }

    fn fs(&self) -> Arc<dyn FileSystem> {
        self.mount_fs.clone()
    }

    fn umount(&self) -> Result<Arc<MountFS>, SyscallErr> {
        if self.is_mountpoint_root() {
            self.mount_fs.umount()?;
            return Ok(self.mount_fs.clone());
        }

        let inode_id = self.inner_inode.metadata()?.inode_id;
        let mounted = {
            let mountpoints = self.mount_fs.mountpoints.lock();
            mountpoints.get(&inode_id).cloned()
        }
        .ok_or(SyscallErr::EINVAL)?;
        mounted.umount()?;
        Ok(mounted)
    }

    fn page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        self.inner_inode.page_cache()
    }

    fn ensure_page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        self.inner_inode.ensure_page_cache()
    }

    fn sync(&self) -> Result<(), SyscallErr> {
        self.inner_inode.sync()
    }

    fn datasync(&self) -> Result<(), SyscallErr> {
        self.inner_inode.datasync()
    }

    fn ioctl(
        &self,
        cmd: u32,
        data: usize,
        private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner_inode.ioctl(cmd, data, private_data)
    }

    fn poll(&self, private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        self.inner_inode.poll(private_data)
    }

    fn read_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        self.inner_inode.read_wait_queue()
    }

    fn read_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        self.inner_inode.read_event_queue()
    }

    fn write_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        self.inner_inode.write_wait_queue()
    }

    fn write_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        self.inner_inode.write_event_queue()
    }

    fn is_stream(&self) -> bool {
        self.inner_inode.is_stream()
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }

    fn absolute_path(&self) -> Result<String, SyscallErr> {
        let mut current = self.self_arc();
        let mut path_parts: Vec<String> = Vec::new();

        loop {
            if current.is_mountpoint_root() && current.mount_fs.self_mountpoint().is_none() {
                break;
            }

            let parent = current.do_parent()?;
            if Arc::ptr_eq(&parent, &current) {
                break;
            }

            // 在 parent 中查找 current 的名称
            let name = parent
                .inner_inode
                .get_entry_name(current.metadata()?.inode_id)
                .unwrap_or_else(|_| alloc::string::String::from("?"));
            path_parts.push(name);

            if path_parts.len() > 64 {
                return Err(SyscallErr::ELOOP);
            }

            current = parent;
        }

        path_parts.reverse();
        let mut absolute_path = String::with_capacity(
            path_parts.iter().map(|s| s.len()).sum::<usize>() + path_parts.len(),
        );
        for part in path_parts {
            absolute_path.push('/');
            absolute_path.push_str(&part);
        }
        if absolute_path.is_empty() {
            absolute_path.push('/');
        }
        Ok(absolute_path)
    }
}

impl Drop for MountFSInode {
    fn drop(&mut self) {
        counters::MOUNTFSINODE_ALIVE.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

// ── MountFS ─────────────────────────────────────────────────────────────

/// MountFS — 挂载感知的文件系统包装器
///
/// 包装一个具体的 `FileSystem`，附加挂载点管理。
/// 对标 DragonOS `kernel/src/filesystem/vfs/mount.rs` 的 `MountFS`。
#[derive(Debug)]
pub struct MountFS {
    /// 内层文件系统
    inner_filesystem: Arc<dyn FileSystem>,
    /// 根 inode
    root_inner_inode: Option<Arc<dyn IndexNode>>,
    /// 子挂载点表: parent_inode_id → mounted fs
    pub mountpoints: Mutex<BTreeMap<InodeId, Arc<MountFS>>>,
    /// 自身挂载到父文件系统上的 inode（如果是根则 None）
    self_mountpoint: Mutex<Option<Arc<MountFSInode>>>,
    /// 挂载标志
    mount_flags: Mutex<MountFlags>,
    /// 挂载源
    mount_source: Mutex<Option<String>>,
    /// 挂载目标路径
    mount_path: Mutex<Option<String>>,
    /// 挂载传播状态
    propagation: MountPropagation,
    /// 指向自身的弱引用
    self_ref: Mutex<Weak<MountFS>>,
    /// Dentry cache: (parent_ino, name) → Arc<MountFSInode>
    pub dentry_cache: Mutex<DentryCache>,
    /// 目录版本号，任何目录修改（create/unlink/rmdir/rename）后递增。
    /// 用于检测并发修改，防止 find() 插入 stale dentry。
    pub dentry_gen: AtomicU64,
    /// 禁用 dentry cache 的动态文件系统（如 procfs）
    pub no_dentry_cache: AtomicBool,
}

impl MountFS {
    /// 创建新的 MountFS
    pub fn new(inner_filesystem: Arc<dyn FileSystem>, mount_flags: MountFlags) -> Arc<Self> {
        counters::MOUNTFS_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Arc::new_cyclic(|self_ref| MountFS {
            root_inner_inode: None,
            inner_filesystem,
            mountpoints: Mutex::new(BTreeMap::new()),
            self_mountpoint: Mutex::new(None),
            mount_flags: Mutex::new(mount_flags),
            mount_source: Mutex::new(None),
            mount_path: Mutex::new(None),
            propagation: MountPropagation::new_private(),
            self_ref: Mutex::new(self_ref.clone()),
            dentry_cache: Mutex::new(DentryCache::new()),
            dentry_gen: AtomicU64::new(0),
            no_dentry_cache: AtomicBool::new(false),
        })
    }

    /// 创建以指定 inode 为根的 MountFS（用于 bind mount）
    pub fn new_with_root(
        inner_filesystem: Arc<dyn FileSystem>,
        root_inner_inode: Arc<dyn IndexNode>,
        mount_flags: MountFlags,
    ) -> Arc<Self> {
        counters::MOUNTFS_ALIVE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        Arc::new_cyclic(|self_ref| MountFS {
            root_inner_inode: Some(root_inner_inode),
            inner_filesystem,
            mountpoints: Mutex::new(BTreeMap::new()),
            self_mountpoint: Mutex::new(None),
            mount_flags: Mutex::new(mount_flags),
            mount_source: Mutex::new(None),
            mount_path: Mutex::new(None),
            propagation: MountPropagation::new_private(),
            self_ref: Mutex::new(self_ref.clone()),
            dentry_cache: Mutex::new(DentryCache::new()),
            dentry_gen: AtomicU64::new(0),
            no_dentry_cache: AtomicBool::new(false),
        })
    }

    /// 获取挂载点根 inode（穿过子挂载表找最底层）
    pub fn mountpoint_root_inode(&self) -> Arc<MountFSInode> {
        let root_inner = self
            .root_inner_inode
            .clone()
            .unwrap_or_else(|| self.inner_filesystem.root_inode());

        let self_arc = self.self_ref.lock().upgrade().unwrap();
        let root_mount_inode = MountFSInode::new(root_inner, self_arc);
        MountFSInode::overlaid_inode(root_mount_inode)
    }

    /// 添加子挂载点
    pub fn add_mount(&self, inode_id: InodeId, mount_fs: Arc<MountFS>) -> Result<(), SyscallErr> {
        let mut mountpoints = self.mountpoints.lock();
        if mountpoints.contains_key(&inode_id) {
            return Err(SyscallErr::EEXIST);
        }
        mountpoints.insert(inode_id, mount_fs);
        Ok(())
    }

    /// 移除子挂载点
    pub fn remove_mount(&self, inode_id: InodeId) -> Option<Arc<MountFS>> {
        self.mountpoints.lock().remove(&inode_id)
    }

    /// 卸载当前文件系统
    pub fn umount(self: &Arc<Self>) -> Result<(), SyscallErr> {
        if !self.mountpoints.lock().is_empty() {
            return Err(SyscallErr::EBUSY);
        }
        // Propagate umount to peers before removing from parent
        if self.propagation().is_shared() {
            if let Some(mp) = self.self_mountpoint.lock().clone() {
                if let Ok(md) = mp.inner_inode.metadata() {
                    propagate_umount(&mp.mount_fs, md.inode_id);
                }
            }
        }
        // Unregister from peer group
        unregister_peer_mount(self);
        // Unregister from global mount list
        if let Some(mountpoint) = self.self_mountpoint.lock().clone() {
            if let Ok(path) = mountpoint.absolute_path() {
                MOUNT_LIST.remove(path);
            }
        }
        if let Some(mountpoint) = self.self_mountpoint.lock().take() {
            if let Ok(md) = mountpoint.inner_inode.metadata() {
                let removed = mountpoint.mount_fs.remove_mount(md.inode_id);
                if removed.is_none() {
                    log::warn!(
                        "MountFS::umount: remove_mount returned None for inode {:?}",
                        md.inode_id
                    );
                }
            }
        }
        // Clear dentry cache to break MountFS → cache → MountFSInode → MountFS cycle
        {
            let evicted = self.dentry_cache.lock().clear_all();
            drop(evicted);
        }
        self.inner_filesystem.on_umount();
        Ok(())
    }

    // ── 属性访问 ───────────────────────────────────────────────────

    pub fn inner_filesystem(&self) -> Arc<dyn FileSystem> {
        self.inner_filesystem.clone()
    }

    pub fn root_inner_inode(&self) -> Arc<dyn IndexNode> {
        self.root_inner_inode
            .clone()
            .unwrap_or_else(|| self.inner_filesystem.root_inode())
    }

    pub fn mount_flags(&self) -> MountFlags {
        *self.mount_flags.lock()
    }

    pub fn set_mount_flags(&self, flags: MountFlags) {
        *self.mount_flags.lock() = flags;
    }

    pub fn self_mountpoint(&self) -> Option<Arc<MountFSInode>> {
        self.self_mountpoint.lock().clone()
    }

    pub fn set_self_mountpoint(&self, mp: Option<Arc<MountFSInode>>) {
        *self.self_mountpoint.lock() = mp;
    }

    pub fn mount_source(&self) -> Option<String> {
        self.mount_source.lock().clone()
    }

    pub fn set_mount_source(&self, source: Option<String>) {
        *self.mount_source.lock() = source;
    }

    pub fn mount_path(&self) -> Option<String> {
        self.mount_path.lock().clone()
    }

    pub fn propagation(&self) -> &MountPropagation {
        &self.propagation
    }

    pub fn set_mount_path(&self, path: Option<String>) {
        *self.mount_path.lock() = path;
    }
}

impl Drop for MountFS {
    fn drop(&mut self) {
        counters::MOUNTFS_ALIVE.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

impl FileSystem for MountFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        self.mountpoint_root_inode()
    }

    fn info(&self) -> super::file_system::FsInfo {
        self.inner_filesystem.info()
    }

    fn name(&self) -> &str {
        self.inner_filesystem.name()
    }

    fn super_block(&self) -> super::file_system::SuperBlock {
        let mut sb = self.inner_filesystem.super_block();
        sb.flags = self.mount_flags().bits() as u64;
        sb
    }

    fn support_readahead(&self) -> bool {
        self.inner_filesystem.support_readahead()
    }

    fn as_any_ref(&self) -> &dyn core::any::Any {
        self
    }
}

// ── MountList ────────────────────────────────────────────────────────────

/// 全局挂载列表
///
/// 管理所有挂载关系（路径 → MountFS），用于路径到挂载点的解析。
#[derive(Debug)]
pub struct MountList {
    /// 挂载路径 → (挂载记录列表，支持 stackable mounts)
    mounts: Mutex<BTreeMap<Arc<MountPath>, Vec<MountRecord>>>,
}

/// 单条挂载记录
#[derive(Debug, Clone)]
struct MountRecord {
    /// 挂载的文件系统
    fs: Arc<MountFS>,
    /// 挂载目标的 inode ID
    ino: Option<InodeId>,
}

impl MountList {
    /// 创建空挂载列表
    pub const fn new() -> Self {
        MountList {
            mounts: Mutex::new(BTreeMap::new()),
        }
    }

    /// 添加挂载点
    pub fn insert<T: Into<MountPath>>(&self, path: T, fs: Arc<MountFS>, ino: Option<InodeId>) {
        let mut inner = self.mounts.lock();
        let path: Arc<MountPath> = Arc::new(path.into());
        let entry = inner.entry(path).or_default();
        entry.push(MountRecord { fs, ino });
    }

    /// 按路径查找挂载点
    /// 返回 `(MountPath, 剩余路径, 挂载的 MountFS)`
    pub fn lookup<T: AsRef<str>>(&self, path: T) -> Option<(Arc<MountPath>, String, Arc<MountFS>)> {
        let inner = self.mounts.lock();
        for (key, stack) in inner.iter().rev() {
            let strkey: &str = &key.0;
            if let Some(rest) = path.as_ref().strip_prefix(strkey) {
                if rest.is_empty() || rest.starts_with('/') {
                    if let Some(rec) = stack.last() {
                        let rest_trimmed = rest.trim_start_matches('/');
                        return Some((key.clone(), rest_trimmed.to_string(), rec.fs.clone()));
                    }
                }
            }
        }
        None
    }

    /// 按路径移除挂载
    pub fn remove<T: Into<MountPath>>(&self, path: T) -> Option<Arc<MountFS>> {
        let mut inner = self.mounts.lock();
        let path: MountPath = path.into();
        if let Some(stack) = inner.get_mut(&path) {
            if let Some(rec) = stack.pop() {
                if stack.is_empty() {
                    inner.remove(&path);
                }
                return Some(rec.fs);
            }
        }
        None
    }
}

// ── Global MountList ─────────────────────────────────────────────────────

lazy_static! {
    /// 全局挂载列表，所有通过 mount_subtree 创建的挂载均在此注册。
    pub static ref MOUNT_LIST: MountList = MountList::new();
}
