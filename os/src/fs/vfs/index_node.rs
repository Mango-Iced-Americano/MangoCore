//! IndexNode trait — VFS 层的 inode 操作抽象
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/mod.rs` 中的 `IndexNode` trait。
//! 所有具体的 inode 实现（ext4、fat32、设备文件、socket 等）都必须实现此 trait。
//!
//! 设计原则：
//! - 默认实现返回 `ENOSYS`（不支持），各具体实现按需覆盖
//! - `read_at` / `write_at` 不带 offset 更新，offset 管理在 `File` 层
//! - `find` 只在当前目录下查找，跨挂载点的逻辑在 `MountFSInode` 中

use crate::utils::error::SyscallErr;
use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;
use core::fmt::Debug;
use spin::MutexGuard;

use super::{
    file::FileFlags, mount::MountFlags, FilePrivateData, FileType, InodeFlags, InodeId, InodeMode,
    Metadata,
};

/// Attributes passed to create_with_attrs for one-shot inode initialization.
#[derive(Clone, Copy, Debug)]
pub struct CreateAttrs {
    pub mode: InodeMode,
    pub uid: u32,
    pub gid: u32,
}

/// IndexNode trait — 所有 inode 实现必须满足的接口
///
/// 方法分为几类：
/// - 基本 I/O：`read_at`、`write_at`、`read_direct`、`write_direct`
/// - 文件生命周期：`open`、`close`
/// - 目录操作：`find`、`list`、`create`、`mkdir`、`link`、`unlink`、`rmdir`
/// - 元数据：`metadata`、`set_metadata`
/// - 大小管理：`resize`、`truncate`
/// - 其他：`fs`、`page_cache`、`ioctl`、`poll`
pub trait IndexNode: Any + Send + Sync + Debug {
    // ── 基本 I/O ────────────────────────────────────────────────────

    /// 在 inode 的指定偏移量处读取数据。
    ///
    /// # Semantics
    ///
    /// 从 `offset` 开始读取最多 `len` 字节到 `buf` 中。
    /// 返回实际读取的字节数（<= len）。到达 EOF 时返回 0。
    /// 偏移量管理由 `File` 层负责，此方法不更新 offset。
    ///
    /// # Locking
    ///
    /// 实现可能持有 inode 内部锁。调用者已将 `data`（`FilePrivateData`）
    /// 锁传入，inode 实现不应在同一路径中再次获取同一把 inode 锁（`TicketMutex` 不可重入）。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。具体实现按需返回 `EIO`、`EINVAL` 等。
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 在 inode 的指定偏移量处写入数据。
    ///
    /// # Semantics
    ///
    /// 从 `offset` 开始写入 `len` 字节。返回值 < len 表示部分写入。
    /// 偏移量管理由 `File` 层负责，此方法不更新 offset。
    /// 写入可能触发 inode 大小扩展（具体由实现决定）。
    ///
    /// # Locking
    ///
    /// 与 `read_at` 相同的锁约束：不可在持有 inode 锁时通过 `File` 层
    /// 回调到同一 inode 的其他方法。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。挂载只读时返回 `EROFS`；空间不足返回 `ENOSPC`。
    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 从 inode 读取数据直连到 UserBuffer，省去 kbuf 中转。
    /// 默认返回 ENOSYS，由 File 层 fallback 到 kbuf 路径。
    /// 有 PageCache 的 inode 应 override 此方法直接调用 PageCache::read_user。
    fn read_at_user(
        &self,
        _offset: usize,
        _len: usize,
        _dst: &mut crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 从 UserBuffer 直连写入 inode，省去 kbuf 中转。
    /// 默认返回 ENOSYS，由 File 层 fallback 到 kbuf 路径。
    fn write_at_user(
        &self,
        _offset: usize,
        _len: usize,
        _src: &crate::mm::UserBuffer,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 是否支持直连 UserBuffer I/O。有 PageCache 的普通文件返回 true；
    /// pipe/socket/devfs/procfs 保持默认 false，继续走 kbuf 路径。
    fn supports_user_buffer_io(&self) -> bool {
        false
    }

    /// 写操作是否丢弃所有数据（如 /dev/null, /dev/zero write）。
    /// 如果返回 true，syscall 层可以在验证权限后直接返回 count，
    /// 跳过 UserBuffer 构造和 copy。
    fn is_discard_write(&self) -> bool {
        false
    }

    /// 丢弃写入：不做任何 I/O，只验证 offset/len 合法性。
    /// 默认通过 is_discard_write() 判断；覆盖此方法可直接返回 Ok(len)。
    fn discard_write_at(
        &self,
        _offset: usize,
        len: usize,
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if self.is_discard_write() {
            Ok(len)
        } else {
            Err(SyscallErr::ENOSYS)
        }
    }

    /// 直接读取（绕过 page cache），用于 O_DIRECT 和回写
    fn read_direct(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 直接写入（绕过 page cache）
    fn write_direct(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 同步读取（page cache 内部使用）
    fn read_sync(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 同步写入（page cache 内部使用）
    fn write_sync(&self, _offset: usize, _buf: &[u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    // ── 文件生命周期 ────────────────────────────────────────────────

    /// 打开文件时调用。
    ///
    /// # Semantics
    ///
    /// 在 `File::new()` 创建 fd 后立即调用，用于执行文件系统特定的打开逻辑
    /// （如 ext4 的 truncate-on-open、设备文件的引用计数管理）。
    ///
    /// # Locking
    ///
    /// 调用时 `data`（`FilePrivateData`）锁已持有。实现应避免长时间持锁。
    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        Ok(()) // 默认：无需特殊处理
    }

    /// 关闭文件时调用。
    ///
    /// # Semantics
    ///
    /// 在最后一个引用释放时调用，用于执行资源清理（如减少设备引用计数、
    /// 释放文件锁、写回脏页等）。
    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        Ok(())
    }

    // ── 目录操作 ────────────────────────────────────────────────────

    /// 在当前目录下查找名为 `name` 的子项。
    ///
    /// # Semantics
    ///
    /// 仅在当前目录范围内查找，不跨越挂载点边界。
    /// 跨挂载点逻辑由 `MountFSInode::find()` 负责。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。未找到返回 `ENOENT`；非目录 inode 返回 `ENOTDIR`。
    fn find(&self, _name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 列出当前目录下的所有子项名称
    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 列出当前目录下的所有子项名称、inode id 和文件类型。
    ///
    /// 默认实现通过 `list()` + `find()` + `metadata()` 兼容所有文件系统；
    /// ext4 等能一次扫描拿到完整目录项信息的实现应覆盖该方法，避免 O(n) 次查找。
    fn list_dirents(&self) -> Result<Vec<(String, InodeId, FileType)>, SyscallErr> {
        let mut result = Vec::new();
        for name in self.list()? {
            if let Ok(child) = self.find(&name) {
                if let Ok(meta) = child.metadata() {
                    result.push((name, meta.inode_id, meta.file_type));
                }
            }
        }
        Ok(result)
    }

    /// 在当前目录下创建常规文件。
    ///
    /// # Semantics
    ///
    /// `file_type` 可以是 `File`、`Dir`、`SymLink` 等。`mode` 为 Unix 权限位。
    /// 返回新创建的 inode 引用。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。名称已存在返回 `EEXIST`；非目录 inode 返回 `ENOTDIR`。
    fn create(
        &self,
        _name: &str,
        _file_type: FileType,
        _mode: InodeMode,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 创建带有额外数据的文件（扩展创建）
    fn create_with_data(
        &self,
        name: &str,
        file_type: FileType,
        mode: InodeMode,
        _data: usize,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        self.create(name, file_type, mode)
    }

    /// 创建文件并一次性设置 uid/gid/mode，避免 post-create set_metadata 二次写。
    /// 默认 fallback 调用 create() 后 set_metadata()，ext4/tmpfs 应 override。
    fn create_with_attrs(
        &self,
        name: &str,
        file_type: FileType,
        attrs: CreateAttrs,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let inode = self.create(name, file_type, attrs.mode)?;
        let mut meta = inode.metadata()?;
        meta.uid = attrs.uid;
        meta.gid = attrs.gid;
        meta.mode = InodeMode::from(file_type) | (attrs.mode & InodeMode::S_IALLUGO);
        if meta.mode.contains(InodeMode::S_ISGID) && attrs.uid != 0 && attrs.uid != attrs.gid {
            meta.mode.remove(InodeMode::S_ISGID);
        }
        inode.set_metadata(&meta).ok();
        Ok(inode)
    }

    /// 创建符号链接
    fn symlink(&self, name: &str, target: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let inode = self.create_with_data(name, FileType::SymLink, InodeMode::S_IRWXUGO, 0)?;
        let bytes = target.as_bytes();
        let len = bytes.len();
        inode.write_at(
            0,
            len,
            bytes,
            spin::Mutex::new(FilePrivateData::Unused).lock(),
        )?;
        Ok(inode)
    }

    /// 在当前目录下创建硬链接。
    ///
    /// # Semantics
    ///
    /// 将 `other` inode 以 `name` 为名链接到当前目录下。
    /// 成功后 `other` 的 `nlinks` 计数增加 1。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。名称已存在返回 `EEXIST`；跨文件系统链接返回 `EXDEV`。
    fn link(&self, _name: &str, _other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 重命名/移动文件或目录到另一个位置。
    ///
    /// # Semantics
    ///
    /// - `old_name`：当前目录下的源文件名
    /// - `new_parent`：目标目录
    /// - `new_name`：目标目录下的新文件名
    /// - `flags`：`renameat2` flags（`RENAME_NOREPLACE`、`RENAME_EXCHANGE` 等）；
    ///   具体覆盖逻辑由各文件系统处理。
    ///
    /// 默认实现：link + unlink（不支持 `RENAME_NOREPLACE` 语义）。
    ///
    /// # Locking
    ///
    /// 实现应确保跨目录 rename 时 dentry cache 一致性。ext4 实现需要 dentry_gen
    /// 版本号递增以失效并发 find 路径的 stale entry。
    ///
    /// # Errors
    ///
    /// `ENOENT`（源不存在）、`ENOTDIR`（非目录）、`EXDEV`（跨文件系统）。
    fn rename(
        &self,
        old_name: &str,
        new_parent: &Arc<dyn IndexNode>,
        new_name: &str,
        _flags: u32,
    ) -> Result<(), SyscallErr> {
        // 默认实现：link + unlink（不支持 RENAME_NOREPLACE 语义）
        let old_inode = self.find(old_name)?;
        new_parent.link(new_name, &old_inode)?;
        self.unlink(old_name)
    }

    /// 在当前目录下删除名为 `name` 的硬链接。
    ///
    /// # Semantics
    ///
    /// 将 `name` 的链接计数减 1。当链接计数降为 0 且无进程持有 fd 时，
    /// 文件数据块可被回收。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。名称不存在返回 `ENOENT`；是目录返回 `EISDIR`。
    fn unlink(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 删除空目录
    fn rmdir(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 创建目录
    fn mkdir(&self, name: &str, mode: InodeMode) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        match self.find(name) {
            Ok(inode) => Err(SyscallErr::EEXIST),
            Err(SyscallErr::ENOENT) => self.create(name, FileType::Dir, mode),
            Err(err) => Err(err),
        }
    }

    // ── 元数据 ──────────────────────────────────────────────────────

    /// 获取 inode 的元数据
    fn metadata(&self) -> Result<Metadata, SyscallErr>;

    /// 设置 inode 的元数据
    fn set_metadata(&self, _metadata: &Metadata) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// Update data modification and inode change timestamps after a successful write.
    fn touch_modified(&self) {
        let Ok(mut metadata) = self.metadata() else {
            return;
        };
        let now = crate::timer::TimeSpec::now();
        metadata.mtime = now;
        metadata.ctime = now;
        let _ = self.set_metadata(&metadata);
    }

    /// 根据 inode 号获取子项的名称
    fn get_entry_name(&self, _ino: InodeId) -> Result<String, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 根据 inode 号获取子项的名称和元数据
    fn get_entry_name_and_metadata(&self, ino: InodeId) -> Result<(String, Metadata), SyscallErr> {
        let name = self.get_entry_name(ino)?;
        let entry = self.find(&name)?;
        Ok((name, entry.metadata()?))
    }

    // ── 大小管理 ────────────────────────────────────────────────────

    /// 调整 inode 大小（扩展或截断）
    fn resize(&self, _len: usize) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 截断 inode 到指定长度
    fn truncate(&self, len: usize) -> Result<(), SyscallErr> {
        self.resize(len)
    }

    // ── 文件系统引用 ────────────────────────────────────────────────

    /// 获取此 inode 所属的文件系统。
    ///
    /// # Semantics
    ///
    /// 返回的 `FileSystem` 引用必须与 inode 生命周期一致。
    /// 用于跨文件系统操作（如 statfs、挂载点定位）。
    fn fs(&self) -> Arc<dyn super::file_system::FileSystem>;

    /// 获取此 inode 的 page cache（如果有）。
    ///
    /// # Semantics
    ///
    /// 只读查询，不创建新 cache。仅当 inode 已有 PageCache 时返回 `Some`。
    /// 用于回写、回收、统计等读取路径——不应触发缓存创建。
    ///
    /// # Locking
    ///
    /// 实现不应在返回 `Option` 时持有 inode 内部锁（避免与 PageCache 的
    /// entries → inner 锁顺序冲突）。
    fn page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        None
    }

    /// 确保此 inode 有 page cache（如果已存在则返回，否则创建）。
    /// 仅 read/write/mmap fault 等真正需要文件数据页的路径调用。
    /// 默认委托给 page_cache()，文件系统可 override 为按需创建。
    fn ensure_page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        self.page_cache()
    }

    // ── 其他操作 ────────────────────────────────────────────────────

    /// I/O 控制。
    ///
    /// # Semantics
    ///
    /// 对标 Linux `ioctl(2)`。`cmd` 为设备特定命令，`data` 为用户空间指针。
    /// 返回值 >= 0 表示成功，负值为 errno。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`。不支持的 `cmd` 返回 `EINVAL` 或 `ENOTTY`。
    fn ioctl(
        &self,
        _cmd: u32,
        _data: usize,
        _private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 返回读端等待队列（可选）。
    ///
    /// # Semantics
    ///
    /// 仅需要阻塞读的设备（Pipe、TTY、Socket 等）实现此方法。
    /// `File::read()` 在遇到 `EAGAIN` 时，将当前任务加入此队列等待数据。
    ///
    /// # Locking
    ///
    /// 返回的 `WaitQueue` 引用在 inode 生命周期内有效。唤醒路径不得在持有
    /// inode 内部锁时操作此队列（防止锁顺序反转）。
    fn read_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        None
    }

    /// 返回读端事件队列（可选）
    /// epoll 使用事件队列建立持久订阅；ppoll/pselect 仍可通过其中的 WaitQueue 睡眠。
    fn read_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        None
    }

    /// 返回写端等待队列（可选）。
    ///
    /// # Semantics
    ///
    /// 仅需要阻塞写的设备（Pipe、Socket 等）实现此方法。
    /// `File::write()` 在遇到 `EAGAIN` 时，将当前任务加入此队列等待缓冲区空间。
    ///
    /// # Locking
    ///
    /// 与 `read_wait_queue` 相同的锁约束。
    fn write_wait_queue(&self) -> Option<&spin::Mutex<crate::task::WaitQueue>> {
        None
    }

    /// 返回写端事件队列（可选）
    fn write_event_queue(&self) -> Option<&super::event::EventWaitQueue> {
        None
    }

    /// 轮询（poll/select/epoll）。
    ///
    /// # Semantics
    ///
    /// 返回文件当前就绪的事件位掩码（`EPollEvent`）。由 `File::poll_events()`
    /// 调用，epoll/poll/select 通过返回值判断 fd 是否可读/可写。
    /// 普通文件默认返回 `EPOLLIN | EPOLLOUT`（始终就绪）。
    ///
    /// # Errors
    ///
    /// 默认返回 `ENOSYS`，`File` 层会 fallback 为始终就绪。
    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 判断是否为"流式"文件（不可 seek，pread/pwrite 应返回 ESPIPE）
    fn is_stream(&self) -> bool {
        false
    }

    /// 返回此 inode 的 fasync 通知列表（用于 SIGIO）。
    /// 仅 pipe / socket 等需要异步 I/O 通知的 inode 需要实现此方法。
    fn fasync_items(&self) -> Option<&super::fasync::FAsyncItems> {
        None
    }

    /// 获取绝对路径
    fn absolute_path(&self) -> Result<String, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 挂载文件系统到此 inode
    fn mount(
        &self,
        _fs: Arc<dyn super::file_system::FileSystem>,
        _mount_flags: MountFlags,
    ) -> Result<Arc<super::mount::MountFS>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 卸载此 inode 下的文件系统
    fn umount(&self) -> Result<Arc<super::mount::MountFS>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 转换为 `Any` 引用，用于向下转型
    fn as_any_ref(&self) -> &dyn Any;

    /// 同步数据到设备
    fn sync(&self) -> Result<(), SyscallErr> {
        Ok(())
    }

    /// 同步数据（不含元数据）
    fn datasync(&self) -> Result<(), SyscallErr> {
        self.sync()
    }

    /// 文件通知 advice
    fn fadvise(&self, _offset: i64, _len: i64, _advise: i32) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 创建特殊文件（设备节点等）
    fn mknod(
        &self,
        _filename: &str,
        _mode: InodeMode,
        _dev_t: u64,
    ) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 获取扩展属性
    /// 返回属性值的字节数；属性不存在→ENODATA；buf太小→ERANGE
    fn getxattr(&self, _name: &str, _buf: &mut [u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 设置扩展属性
    /// flags: XATTR_CREATE(1) / XATTR_REPLACE(2) / 0
    fn setxattr(&self, _name: &str, _value: &[u8], _flags: u32) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 列出所有扩展属性的名称（null-separated）
    /// 返回名称列表的总字节数；buf太小→ERANGE
    fn listxattr(&self, _buf: &mut [u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 删除扩展属性
    /// 返回 0 表示成功；属性不存在→ENODATA
    fn removexattr(&self, _name: &str) -> Result<usize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }
}

/// 实现 `Any` 向下转型的便捷宏
#[macro_export]
macro_rules! impl_index_node_as_any {
    ($t:ty) => {
        fn as_any_ref(&self) -> &dyn ::core::any::Any {
            self
        }
    };
}
