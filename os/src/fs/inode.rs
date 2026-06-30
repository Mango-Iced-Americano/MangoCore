use crate::fs::*;
use core::any::Any;

use crate::fs::fat32::layout::FATDiskInodeType;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fat32::fat_inode::FileContent;
use fat32::layout::FATShortDirEnt;
use spin::{Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};

/// 标记类型 — 表示当前持有一个独占的 inode 锁，用于需要持有 inode 内部
/// `Mutex` 才能执行的操作。本身无数据，仅作为编译期守卫。
pub struct InodeLock;

#[allow(unused)]
/// inode 时间戳容器。
///
/// 时间值以纳秒为单位，基于系统启动后的单调时钟（非 Unix epoch）。
/// 与 VFS 层的 `InodeTime` 交互由文件系统具体实现负责同步。
pub struct InodeTime {
    create_time: u64,
    access_time: u64,
    modify_time: u64,
}
#[allow(unused)]
impl InodeTime {
    pub fn new() -> Self {
        Self {
            create_time: 0,
            access_time: 0,
            modify_time: 0,
        }
    }
    /// 设置 inode 创建时间（纳秒，系统启动后的单调时间戳）。
    pub fn set_create_time(&mut self, create_time: u64) {
        self.create_time = create_time;
    }

    /// 返回创建时间的不可变引用。
    pub fn create_time(&self) -> &u64 {
        &self.create_time
    }

    /// 设置 inode 最后访问时间（纳秒）。
    pub fn set_access_time(&mut self, access_time: u64) {
        self.access_time = access_time;
    }

    /// 返回最后访问时间的不可变引用。
    pub fn access_time(&self) -> &u64 {
        &self.access_time
    }

    /// 设置 inode 最后修改时间（纳秒）。
    pub fn set_modify_time(&mut self, modify_time: u64) {
        self.modify_time = modify_time;
    }

    /// 返回最后修改时间的不可变引用。
    pub fn modify_time(&self) -> &u64 {
        &self.modify_time
    }
}

// 文件或者目录
#[derive(PartialEq, Debug, Clone, Copy)]
pub enum DiskInodeType {
    File,
    Directory,
    FIFO,
    Character,
    Block,
    Socket,
    Link,
    /// Unknown/invalid inode type (e.g. corrupted disk metadata)
    Unknown,
}
