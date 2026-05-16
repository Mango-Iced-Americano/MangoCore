//! DirtyBlockDevice — 元数据脏块缓存包装器
//!
//! 包装真实的 BlockDevice，拦截 read_block/write_block：
//! - write_block: 写入内存缓存（延迟落盘）
//! - read_block:  优先从缓存读，未命中再读真实设备
//! - flush_dirty_blocks(): 一次性刷所有脏块到真实设备
//!
//! 设计目的：消除 busybox --install 等 metadata-heavy 操作的写放大
//! （同一目录块/inode 表块被反复读改写 300+ 次 → 合并为一次读一次写）

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use spin::Mutex;

use crate::drivers::block::BlockDevice;
use crate::hal::BLOCK_SZ;

pub struct DirtyBlockDevice {
    inner: Arc<dyn BlockDevice>,
    dirty: Mutex<BTreeMap<usize, Vec<u8>>>,
}

impl DirtyBlockDevice {
    pub fn new(inner: Arc<dyn BlockDevice>) -> Self {
        DirtyBlockDevice {
            inner,
            dirty: Mutex::new(BTreeMap::new()),
        }
    }

    /// 刷所有脏块到真实块设备
    pub fn flush_dirty_blocks(&self) {
        let dirty_blocks: BTreeMap<usize, Vec<u8>> = {
            let mut guard = self.dirty.lock();
            core::mem::take(&mut *guard)
        };
        for (block_id, data) in dirty_blocks {
            self.inner.write_block(block_id, &data);
        }
    }
}

impl BlockDevice for DirtyBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) {
        // 先查脏缓存
        {
            let dirty = self.dirty.lock();
            if let Some(cached) = dirty.get(&block_id) {
                let len = buf.len().min(cached.len());
                buf[..len].copy_from_slice(&cached[..len]);
                return;
            }
        }
        // 未命中 → 读真实设备
        self.inner.read_block(block_id, buf);
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) {
        // 写入内存缓存（不落盘）
        let mut dirty = self.dirty.lock();
        dirty.insert(block_id, buf.to_vec());
    }

    fn clear_block(&self, block_id: usize, num: u8) {
        let mut dirty = self.dirty.lock();
        dirty.insert(block_id, vec![num; BLOCK_SZ]);
    }

    fn clear_mult_block(&self, block_id: usize, cnt: usize, num: u8) {
        let mut dirty = self.dirty.lock();
        for i in block_id..block_id + cnt {
            dirty.insert(i, vec![num; BLOCK_SZ]);
        }
    }
}
