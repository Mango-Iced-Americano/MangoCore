//! IndexNode trait — VFS 层的 inode 操作抽象
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/mod.rs` 中的 `IndexNode` trait。
//! 所有具体的 inode 实现（ext4、fat32、设备文件、socket 等）都必须实现此 trait。
//!
//! 设计原则：
//! - 默认实现返回 `ENOSYS`（不支持），各具体实现按需覆盖
//! - `read_at` / `write_at` 不带 offset 更新，offset 管理在 `File` 层
//! - `find` 只在当前目录下查找，跨挂载点的逻辑在 `MountFSInode` 中

use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;
use core::fmt::Debug;
use spin::MutexGuard;
use crate::utils::error::SyscallErr;

use super::{
    file::FileFlags, mount::MountFlags, FilePrivateData, FileType, InodeFlags, InodeId, InodeMode,
    Metadata,
};

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

    /// 在 inode 的指定偏移量处读取数据
    ///
    /// # 参数
    /// - `offset`: 读取起始位置
    /// - `len`: 要读取的字节数
    /// - `buf`: 目标缓冲区（buf.len() >= len）
    /// - `data`: 文件私有数据
    ///
    /// # 返回
    /// - 成功: `Ok(读取的字节数)`
    /// - 失败: `Err(SyscallErr)`
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &mut [u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 在 inode 的指定偏移量处写入数据
    ///
    /// # 参数
    /// - `offset`: 写入起始位置
    /// - `len`: 要写入的字节数
    /// - `buf`: 源缓冲区
    /// - `data`: 文件私有数据
    ///
    /// # 返回
    /// - 成功: `Ok(写入的字节数)`
    /// - 失败: `Err(SyscallErr)`
    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        _buf: &[u8],
        _data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
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

    /// 打开文件时调用
    fn open(
        &self,
        _data: MutexGuard<FilePrivateData>,
        _flags: &FileFlags,
    ) -> Result<(), SyscallErr> {
        Ok(()) // 默认：无需特殊处理
    }

    /// 关闭文件时调用
    fn close(&self, _data: MutexGuard<FilePrivateData>) -> Result<(), SyscallErr> {
        Ok(())
    }

    // ── 目录操作 ────────────────────────────────────────────────────

    /// 在当前目录下查找名为 `name` 的子项
    fn find(&self, _name: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 列出当前目录下的所有子项名称
    fn list(&self) -> Result<Vec<String>, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 在当前目录下创建普通文件
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

    /// 创建符号链接
    fn symlink(&self, name: &str, target: &str) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        let inode =
            self.create_with_data(name, FileType::SymLink, InodeMode::S_IRWXUGO, 0)?;
        let bytes = target.as_bytes();
        let len = bytes.len();
        inode.write_at(0, len, bytes, spin::Mutex::new(FilePrivateData::Unused).lock())?;
        Ok(inode)
    }

    /// 在当前目录下创建硬链接
    fn link(&self, _name: &str, _other: &Arc<dyn IndexNode>) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 在当前目录下删除名为 `name` 的硬链接
    fn unlink(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 删除空目录
    fn rmdir(&self, _name: &str) -> Result<(), SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 创建目录
    fn mkdir(&self, name: &str, mode: InodeMode) -> Result<Arc<dyn IndexNode>, SyscallErr> {
        // 默认实现：先 find 再 create
        match self.find(name) {
            Ok(inode) => {
                if inode.metadata()?.file_type == FileType::Dir {
                    Ok(inode)
                } else {
                    Err(SyscallErr::EEXIST)
                }
            }
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

    /// 根据 inode 号获取子项的名称
    fn get_entry_name(&self, _ino: InodeId) -> Result<String, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 根据 inode 号获取子项的名称和元数据
    fn get_entry_name_and_metadata(
        &self,
        ino: InodeId,
    ) -> Result<(String, Metadata), SyscallErr> {
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

    /// 获取此 inode 所属的文件系统
    fn fs(&self) -> Arc<dyn super::file_system::FileSystem>;

    /// 获取此 inode 的 page cache（如果有）
    fn page_cache(&self) -> Option<Arc<super::super::page_cache::PageCache>> {
        None
    }

    // ── 其他操作 ────────────────────────────────────────────────────

    /// I/O 控制
    fn ioctl(
        &self,
        _cmd: u32,
        _data: usize,
        _private_data: MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 轮询（poll/select/epoll）
    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 判断是否为"流式"文件（不可 seek，pread/pwrite 应返回 ESPIPE）
    fn is_stream(&self) -> bool {
        false
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
    fn fadvise(
        &self,
        _offset: i64,
        _len: i64,
        _advise: i32,
    ) -> Result<usize, SyscallErr> {
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
    fn getxattr(&self, _name: &str, _buf: &mut [u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
    }

    /// 设置扩展属性
    fn setxattr(&self, _name: &str, _value: &[u8]) -> Result<usize, SyscallErr> {
        Err(SyscallErr::ENOSYS)
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
