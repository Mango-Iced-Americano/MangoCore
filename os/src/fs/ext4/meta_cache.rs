use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;

pub struct MetaBlockCache {
    blocks: Mutex<BTreeMap<usize, CachedBlock>>,
    max_capacity: usize,
    block_size: usize,
    lru_clock: Mutex<u64>,
}

struct CachedBlock {
    data: Vec<u8>,
    dirty: bool,
    last_used: u64,
}

impl MetaBlockCache {
    pub fn new(max_capacity: usize, block_size: usize) -> Self {
        Self {
            blocks: Mutex::new(BTreeMap::new()),
            max_capacity,
            block_size,
            lru_clock: Mutex::new(0),
        }
    }

    pub fn read_block(
        &self,
        block_id: usize,
        read_from_device: impl FnOnce(usize, &mut [u8]),
    ) -> Vec<u8> {
        let mut blocks = self.blocks.lock();
        let now = self.next_lru_tick();
        if let Some(cached) = blocks.get_mut(&block_id) {
            cached.last_used = now;
            super::counters::inc_counter!(super::counters::METADATA_BLOCK_CACHE_HIT);
            return cached.data.clone();
        }
        if blocks.len() >= self.max_capacity {
            self.evict_one(&mut blocks);
        }
        super::counters::inc_counter!(super::counters::METADATA_BLOCK_CACHE_MISS);
        let mut data = alloc::vec![0u8; self.block_size];
        read_from_device(block_id, &mut data);
        blocks.insert(
            block_id,
            CachedBlock {
                data: data.clone(),
                dirty: false,
                last_used: now,
            },
        );
        data
    }

    pub fn with_block_mut(
        &self,
        block_id: usize,
        read_from_device: impl FnOnce(usize, &mut [u8]),
        f: impl FnOnce(&mut [u8]),
    ) {
        let mut blocks = self.blocks.lock();
        let now = self.next_lru_tick();
        if blocks.len() >= self.max_capacity && !blocks.contains_key(&block_id) {
            self.evict_one(&mut blocks);
        }
        let entry = blocks.entry(block_id).or_insert_with(|| {
            super::counters::inc_counter!(super::counters::METADATA_BLOCK_CACHE_MISS);
            let mut data = alloc::vec![0u8; self.block_size];
            read_from_device(block_id, &mut data);
            CachedBlock {
                data,
                dirty: false,
                last_used: now,
            }
        });
        entry.last_used = now;
        f(&mut entry.data);
        if !entry.dirty {
            super::counters::inc_counter!(super::counters::METADATA_DIRTY_BLOCK_COUNT);
        }
        entry.dirty = true;
    }

    pub fn store_dirty_block(&self, block_id: usize, data: &[u8]) {
        let mut blocks = self.blocks.lock();
        let now = self.next_lru_tick();
        if blocks.len() >= self.max_capacity && !blocks.contains_key(&block_id) {
            self.evict_one(&mut blocks);
        }
        let entry = blocks.entry(block_id).or_insert_with(|| CachedBlock {
            data: alloc::vec![0u8; self.block_size],
            dirty: false,
            last_used: now,
        });
        let copy_len = core::cmp::min(entry.data.len(), data.len());
        entry.data[..copy_len].copy_from_slice(&data[..copy_len]);
        entry.last_used = now;
        if !entry.dirty {
            super::counters::inc_counter!(super::counters::METADATA_DIRTY_BLOCK_COUNT);
        }
        entry.dirty = true;
    }

    pub fn mark_dirty(&self, block_id: usize) {
        let mut blocks = self.blocks.lock();
        let now = self.next_lru_tick();
        if let Some(cached) = blocks.get_mut(&block_id) {
            cached.last_used = now;
            if !cached.dirty {
                super::counters::inc_counter!(super::counters::METADATA_DIRTY_BLOCK_COUNT);
            }
            cached.dirty = true;
        }
    }

    pub fn flush_block(&self, block_id: usize, write_to_device: impl FnOnce(usize, &[u8])) {
        let mut blocks = self.blocks.lock();
        if let Some(cached) = blocks.get_mut(&block_id) {
            if cached.dirty {
                write_to_device(block_id, &cached.data);
                cached.dirty = false;
                super::counters::inc_counter!(super::counters::METADATA_FLUSH_COUNT);
                super::counters::inc_counter!(super::counters::METADATA_BLOCK_WRITE_COUNT);
            }
        }
    }

    pub fn flush_all_dirty(&self, write_to_device: impl Fn(usize, &[u8])) {
        let mut blocks = self.blocks.lock();
        let mut dirty_ids: Vec<usize> = blocks
            .iter()
            .filter(|(_, b)| b.dirty)
            .map(|(id, _)| *id)
            .collect();
        dirty_ids.sort_unstable();
        let sb_blocks: Vec<usize> = dirty_ids.iter().filter(|id| **id <= 1).copied().collect();
        let other_blocks: Vec<usize> = dirty_ids.iter().filter(|id| **id > 1).copied().collect();
        for id in other_blocks.iter().chain(sb_blocks.iter()) {
            if let Some(cached) = blocks.get_mut(id) {
                if cached.dirty {
                    write_to_device(*id, &cached.data);
                    cached.dirty = false;
                    super::counters::inc_counter!(super::counters::METADATA_FLUSH_COUNT);
                    super::counters::inc_counter!(super::counters::METADATA_BLOCK_WRITE_COUNT);
                }
            }
        }
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let blocks = self.blocks.lock();
        let len = blocks.len();
        let dirty = blocks.iter().filter(|(_, b)| b.dirty).count();
        (len, dirty, len - dirty)
    }

    fn evict_one(&self, blocks: &mut BTreeMap<usize, CachedBlock>) {
        let to_evict = blocks
            .iter()
            .filter(|(_, block)| !block.dirty)
            .min_by_key(|(_, block)| block.last_used)
            .map(|(id, _)| *id);
        if let Some(id) = to_evict {
            blocks.remove(&id);
        }
    }

    fn next_lru_tick(&self) -> u64 {
        let mut clock = self.lru_clock.lock();
        *clock = clock.wrapping_add(1);
        *clock
    }
}
