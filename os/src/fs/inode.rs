use crate::fs::*;
use core::any::Any;

use crate::fs::fat32::layout::FATDiskInodeType;
use alloc::sync::Arc;
use alloc::vec::Vec;
use fat32::fat_inode::FileContent;
use fat32::layout::FATShortDirEnt;
use spin::{Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};

pub struct InodeLock;

#[allow(unused)]

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
    /// 设置inode的创建时间
    pub fn set_create_time(&mut self, create_time: u64) {
        self.create_time = create_time;
    }

    /// 获取inode的创建时间的引用
    pub fn create_time(&self) -> &u64 {
        &self.create_time
    }

    /// 设置inode的访问时间
    pub fn set_access_time(&mut self, access_time: u64) {
        self.access_time = access_time;
    }

    /// 获取inode的访问时间的引用
    pub fn access_time(&self) -> &u64 {
        &self.access_time
    }

    /// 设置inode的修改时间
    pub fn set_modify_time(&mut self, modify_time: u64) {
        self.modify_time = modify_time;
    }

    /// 获取inode的修改时间的引用
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
