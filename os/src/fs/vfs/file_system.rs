//! FileSystem trait — 具体文件系统的抽象接口
//!
//! 参考 DragonOS `kernel/src/filesystem/vfs/mod.rs` 中的 `FileSystem` trait 设计。
//! 每个具体的文件系统实现（ext4、fat32、tmpfs、devfs 等）都必须实现此 trait。

use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::{any::Any, fmt::Debug};

use super::index_node::IndexNode;

/// 文件系统信息
#[derive(Debug)]
pub struct FsInfo {
    /// 文件系统所在的块设备 ID
    pub blk_dev_id: usize,
    /// 文件名的最大长度
    pub max_name_len: usize,
    /// 文件系统支持的特性
    pub features: Vec<&'static str>,
}

/// SuperBlock 信息（用于 statfs）
#[derive(Debug, Clone)]
pub struct SuperBlock {
    /// 文件系统类型标识
    pub f_type: u64,
    /// 块大小
    pub f_bsize: u64,
    /// 总块数
    pub f_blocks: u64,
    /// 空闲块数
    pub f_bfree: u64,
    /// 可用块数（非 root）
    pub f_bavail: u64,
    /// 总 inode 数
    pub f_files: u64,
    /// 空闲 inode 数
    pub f_ffree: u64,
    /// 文件系统 ID
    pub f_fsid: [i32; 2],
    /// 文件名最大长度
    pub f_namelen: u64,
    /// 片段大小
    pub f_frsize: u64,
    /// 挂载标志
    pub flags: u64,
    /// 空闲空间（不同 FS 含义不同）
    pub f_spare: [u64; 4],
}

impl Default for SuperBlock {
    fn default() -> Self {
        SuperBlock {
            f_type: 0,
            f_bsize: 4096,
            f_blocks: 0,
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_fsid: [0; 2],
            f_namelen: 255,
            f_frsize: 4096,
            flags: 0,
            f_spare: [0; 4],
        }
    }
}

/// VFS 权限检查策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsPermissionPolicy {
    /// 标准 Unix DAC 权限检查
    Dac,
    /// 远程文件系统（如 FUSE），权限由远端决定
    Remote,
}

/// FileSystem trait — 所有具体文件系统都必须实现
///
/// 参考 DragonOS `kernel/src/filesystem/vfs/mod.rs` 中的 `FileSystem` trait 设计。
///
/// # Limitations
///
/// 相比 DragonOS 参考实现，以下高级特性未支持：
/// - `fault` / `map_pages`：VMA 反向映射和原地缺页处理 —— 当前内核仅使用
///   `PageCache` 驱动的缺页方式，不需要 inode 级别的 `fault` 回调。
///   Exit condition: 引入 DAX 或 GPU 直接映射等需要 inode 级别 fault 处理的场景。
/// - `permission_policy` 配置化：权限检查固定使用标准 Unix DAC（
///   `FsPermissionPolicy::Dac`）。`Remote` 变体预留用于未来的 FUSE 支持。
/// - 异步 I/O 和 `O_DIRECT` 标志穿透 —— 当前所有 I/O 均经 `PageCache`，
///   不直接操作块设备。
pub trait FileSystem: Any + Send + Sync + Debug {
    /// Return a boot-lifetime identity for this filesystem instance.
    ///
    /// This is an internal key, not a userspace device number.  Wrappers such
    /// as `MountFS` must delegate to their backing filesystem so bind mounts
    /// of the same inode retain one identity.
    fn identity_key(&self) -> usize {
        self as *const Self as *const () as usize
    }

    /// 返回文件系统的根 `IndexNode`。
    ///
    /// 该 inode 的生命周期与文件系统挂载一致，多次调用返回同一个 `Arc`。
    fn root_inode(&self) -> Arc<dyn IndexNode>;

    /// 返回文件系统的静态信息（块设备 ID、最大文件名长度、支持的特性列表）。
    fn info(&self) -> FsInfo;

    /// 返回文件系统类型名称（如 `"ext4"`、`"fat32"`），用于日志和诊断。
    fn name(&self) -> &str;

    /// 返回文件系统的超级块信息（用于 `statfs`）。
    ///
    /// 默认实现返回 `SuperBlock::default()`（字段全为零，块大小为 4096）。
    /// 具体文件系统应覆盖此方法，从磁盘超级块填充真实值。
    fn super_block(&self) -> SuperBlock {
        SuperBlock::default()
    }

    /// 获取针对特定 inode 的 statfs 信息
    fn statfs(&self, _inode: &Arc<dyn IndexNode>) -> Result<SuperBlock, SyscallErr> {
        Ok(self.super_block())
    }

    /// 文件系统是否支持预读（readahead）
    fn support_readahead(&self) -> bool {
        true
    }

    /// VFS 权限检查策略
    fn permission_policy(&self) -> FsPermissionPolicy {
        FsPermissionPolicy::Dac
    }

    /// Persist dirty data and metadata belonging to this filesystem instance.
    ///
    /// Memory-only filesystems need no durability work. Persistent backends
    /// must override this method; backends that do not yet keep an
    /// instance-local cache registry may explicitly use the global PageCache
    /// compatibility path in their implementation.
    fn sync(&self) -> Result<(), SyscallErr> {
        Ok(())
    }

    /// 卸载后回调。
    ///
    /// 后端必须在所有必要的写回和资源脱钩都成功后才返回
    /// `Ok(())`。返回错误时，VFS 会保留后端的 `Dying` 状态并在后续
    /// drain 中重试，避免将尚未完全卸载的文件系统误标为 `Dead`。
    fn on_umount(&self) -> Result<(), SyscallErr> {
        self.sync()
    }

    /// 转换为 Any 引用（用于向下转型）
    fn as_any_ref(&self) -> &dyn Any;
}
