//! 基于内存压缩页的简易 zram 后端。
//!
//! OOM 回收路径把匿名页压缩成 `Vec<u8>` 并交给全局 `ZRAM_DEVICE` 保存；
//! `ZramTracker` 通过 RAII 在最后一个引用释放时回收槽位。
//!
//! # Locking
//!
//! 全局设备由 `Mutex<Zram>` 保护。调用者不应在持有 VMA/page table 锁并可能递归进入
//! OOM 回收时长期占用该锁。

use alloc::{sync::Arc, vec::Vec};
use lazy_static::lazy_static;
use lz4_flex::{compress_prepend_size, decompress_size_prepended};
use spin::Mutex;

/// Zram错误枚举
#[derive(Debug)]
pub enum ZramError {
    /// 无效索引
    InvalidIndex,
    /// 空间不足
    NoSpace,
    /// 未分配
    NotAllocated,
}

/// 一个 zram 槽位的 RAII 跟踪器。
#[derive(Debug)]
pub struct ZramTracker(pub usize);

impl Drop for ZramTracker {
    /// 自动释放Zram资源
    fn drop(&mut self) {
        ZRAM_DEVICE.lock().discard(self.0).unwrap();
    }
}

/// 固定容量的压缩页存储。
pub struct Zram {
    /// 压缩数据存储
    compressed: Vec<Option<Vec<u8>>>,
    /// 回收的索引
    recycled: Vec<u16>,
    /// 当前分配的位置
    tail: u16,
}

impl Zram {
    /// 创建一个最多保存 `capacity` 个压缩页的设备。
    pub fn new(capacity: usize) -> Self {
        let mut compressed = Vec::with_capacity(capacity);
        compressed.resize(compressed.capacity(), None);
        Self {
            compressed,
            recycled: Vec::new(),
            tail: 0,
        }
    }

    /// 插入一个已经压缩好的页，并返回对应槽位跟踪器。
    fn insert(&mut self, data: Vec<u8>) -> Result<Arc<ZramTracker>, ZramError> {
        let zram_id = match self.recycled.pop() {
            Some(zram_id) => zram_id as usize,
            None => {
                if self.tail as usize == self.compressed.len() {
                    return Err(ZramError::NoSpace);
                } else {
                    self.tail += 1;
                    (self.tail - 1) as usize
                }
            }
        };
        self.compressed[zram_id] = Some(data);
        Ok(Arc::new(ZramTracker(zram_id)))
    }

    /// 获取压缩数据。
    fn get(&self, zram_id: usize) -> Result<&Vec<u8>, ZramError> {
        if zram_id >= self.compressed.len() {
            return Err(ZramError::InvalidIndex);
        }
        match &self.compressed[zram_id] {
            Some(compressed_data) => Ok(compressed_data),
            None => Err(ZramError::NotAllocated),
        }
    }

    /// 移除一个槽位并回收索引。
    fn remove(&mut self, zram_id: usize) -> Result<Vec<u8>, ZramError> {
        if zram_id >= self.compressed.len() {
            return Err(ZramError::InvalidIndex);
        }
        if zram_id == (self.tail - 1) as usize {
            self.tail = zram_id as u16;
        } else {
            self.recycled.push(zram_id as u16);
        }
        match self.compressed[zram_id].take() {
            Some(compressed_data) => Ok(compressed_data),
            None => Err(ZramError::NotAllocated),
        }
    }

    /// 解压指定槽位到 `buf`。
    ///
    /// # Errors
    ///
    /// 槽位越界或未分配时返回 `ZramError`。压缩数据损坏会触发底层 lz4 panic，
    /// 因为当前 zram 只保存内核自己产生的数据。
    pub fn read(&mut self, zram_id: usize, buf: &mut [u8]) -> Result<(), ZramError> {
        match self.get(zram_id) {
            Ok(compressed_data) => {
                let decompressed_data =
                    decompress_size_prepended(compressed_data.as_slice()).unwrap();
                buf.copy_from_slice(decompressed_data.as_slice());
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 压缩一个页并写入 zram。
    pub fn write(&mut self, buf: &[u8]) -> Result<Arc<ZramTracker>, ZramError> {
        let mut compressed = compress_prepend_size(buf);
        compressed.shrink_to_fit();
        log::trace!("[zram] compressed len: {}", compressed.len());
        self.insert(compressed)
    }
    #[inline(always)]
    /// 释放一个 zram 槽位。
    pub fn discard(&mut self, zram_id: usize) -> Result<(), ZramError> {
        match self.remove(zram_id) {
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

lazy_static! {
    /// 全局ZRAM设备
    pub static ref ZRAM_DEVICE: Arc<Mutex<Zram>> = Arc::new(Mutex::new(Zram::new(2048)));
}
