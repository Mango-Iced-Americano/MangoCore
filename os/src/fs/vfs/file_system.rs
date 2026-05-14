//! FileSystem trait — 具体文件系统的抽象接口
//!
//! 对标 DragonOS `kernel/src/filesystem/vfs/mod.rs` 中的 `FileSystem` trait。
//! 每个具体的文件系统实现（ext4、fat32、tmpfs、devfs 等）都必须实现此 trait。

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::{any::Any, fmt::Debug};
use crate::utils::error::SyscallErr;

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
/// 注意：这是一个精简版，对标 DragonOS 但去掉了我们暂时不需要的功能
/// （如 `fault`、`map_pages`、`permission_policy` 等高级特性）。
pub trait FileSystem: Any + Send + Sync + Debug {
    /// 获取文件系统的根 inode
    fn root_inode(&self) -> Arc<dyn IndexNode>;

    /// 获取文件系统信息
    fn info(&self) -> FsInfo;

    /// 获取文件系统的名称（如 "ext4"、"fat32"）
    fn name(&self) -> &str;

    /// 获取超级块信息
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

    /// 卸载后回调
    fn on_umount(&self) {}

    /// 转换为 Any 引用（用于向下转型）
    fn as_any_ref(&self) -> &dyn Any;
}
