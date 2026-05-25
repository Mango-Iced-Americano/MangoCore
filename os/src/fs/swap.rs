use alloc::{sync::Arc, vec::Vec};
use spin::Mutex;

use crate::{config::PAGE_SIZE, drivers::BLOCK_DEVICE, hal::BLOCK_SZ, mm::MemoryError};

use lazy_static::*;

lazy_static! {
    pub static ref SWAP_DEVICE: Mutex<Swap> = Mutex::new(Swap::new(16));
}

#[derive(Debug)]
pub struct SwapTracker(pub usize);

impl Drop for SwapTracker {
    fn drop(&mut self) {
        // discard() 里 clear_bit + get_block_ids 都不依赖 block_ids 非空
        SWAP_DEVICE.lock().discard(self.0);
    }
}

pub struct Swap {
    bitmap: Vec<u64>,
    block_ids: Vec<usize>,
}
const BLK_PER_PG: usize = PAGE_SIZE / BLOCK_SZ;
const SWAP_SIZE: usize = 1024 * 1024;
impl Swap {
    /// size: the number of megabytes in swap
    pub fn new(size: usize) -> Self {
        let bit = size * (SWAP_SIZE / PAGE_SIZE); // 1MiB = 4KiB*256
        let vec_len = bit / usize::MAX.count_ones() as usize;
        let mut bitmap = Vec::<u64>::with_capacity(vec_len);
        bitmap.resize(bitmap.capacity(), 0);
        Self {
            bitmap,
            block_ids: Vec::new(), // TODO: implement block alloc without old VFS
        }
    }

    /// Returns true only when block_ids are populated enough to back all swap slots.
    fn active(&self) -> bool {
        !self.block_ids.is_empty() && self.block_ids.len() >= self.bitmap.len() * 64 * BLK_PER_PG
    }

    fn read_page(block_ids: &[usize], buf: &mut [u8]) {
        if block_ids.is_empty() {
            return;
        }
        assert!(block_ids[0] + BLK_PER_PG - 1 == block_ids[BLK_PER_PG - 1]);
        BLOCK_DEVICE.read_block(block_ids[0], buf);
    }
    fn write_page(block_ids: &[usize], buf: &[u8]) {
        if block_ids.is_empty() {
            return;
        }
        assert!(block_ids[0] + (BLK_PER_PG - 1) == block_ids[BLK_PER_PG - 1]);
        BLOCK_DEVICE.write_block(block_ids[0], buf);
    }
    fn set_bit(&mut self, pos: usize) {
        self.bitmap[pos / 64] |= 1 << (pos % 64);
    }
    fn clear_bit(&mut self, pos: usize) {
        self.bitmap[pos / 64] &= !(1 << (pos % 64));
    }
    fn alloc_page(&self) -> Option<usize> {
        for (i, bit) in self.bitmap.iter().enumerate() {
            if *bit == u64::MAX {
                continue; // 所有 64 位都已被占用，跳过
            }
            let free_bit = (!*bit).trailing_zeros() as usize;
            return Some(i * 64 + free_bit);
        }
        None
    }
    fn get_block_ids(&self, swap_id: usize) -> Option<&[usize]> {
        let start = swap_id.checked_mul(BLK_PER_PG)?;
        let end = start.checked_add(BLK_PER_PG)?;
        if end > self.block_ids.len() {
            return None;
        }
        Some(&self.block_ids[start..end])
    }

    /// Reads a swapped page back into memory.
    ///
    /// Returns `Err(BackingStoreFailure)` if swap is inactive or the swap_id is invalid.
    pub fn read(&mut self, swap_id: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        if !self.active() {
            return Err(MemoryError::BackingStoreFailure);
        }
        let block_ids = self
            .get_block_ids(swap_id)
            .ok_or(MemoryError::BackingStoreFailure)?;
        Self::read_page(block_ids, buf);
        Ok(())
    }

    /// Writes a page to swap, returning a tracker that frees the slot on drop.
    ///
    /// Returns `Err(OutOfMemory)` if swap is inactive or full.
    pub fn write(&mut self, buf: &[u8]) -> Result<Arc<SwapTracker>, MemoryError> {
        if !self.active() {
            return Err(MemoryError::OutOfMemory);
        }
        let swap_id = self.alloc_page().ok_or(MemoryError::OutOfMemory)?;
        let block_ids = self
            .get_block_ids(swap_id)
            .ok_or(MemoryError::BackingStoreFailure)?;
        Self::write_page(block_ids, buf);
        self.set_bit(swap_id);
        Ok(Arc::new(SwapTracker(swap_id)))
    }

    #[inline(always)]
    pub fn discard(&mut self, swap_id: usize) {
        self.clear_bit(swap_id);
    }
}
