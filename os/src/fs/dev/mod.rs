pub mod hwclock;
pub mod null;
pub mod pipe;
pub mod socket;
pub mod tty;
pub mod zero;
pub mod urandom;

use alloc::sync::Arc;
use core::any::Any;
use lazy_static::*;

use crate::fs::vfs::file_system::{FileSystem, FsInfo, SuperBlock};
use crate::fs::vfs::IndexNode;

/// 设备文件系统（DevFS）— 所有设备文件的虚拟文件系统
#[derive(Debug)]
pub struct DevFS;

impl FileSystem for DevFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        panic!("DevFS has no root inode")
    }
    fn info(&self) -> FsInfo {
        FsInfo {
            blk_dev_id: 0,
            max_name_len: 255,
            features: alloc::vec!["devfs"],
        }
    }
    fn name(&self) -> &str {
        "devfs"
    }
    fn super_block(&self) -> SuperBlock {
        SuperBlock::default()
    }
    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

lazy_static! {
    /// 共享的 DevFS 实例，设备文件使用
    pub static ref DEV_FS: Arc<DevFS> = Arc::new(DevFS);
}

#[macro_export]
macro_rules! makedev {
    ($x:literal, $y:literal) => {
        (($x & 0xfffff000) << 32)
            | (($x & 0x00000fff) << 8)
            | (($y & 0xffffff00) << 12)
            | ($y & 0x000000ff)
    };
}
