use super::{
    bitmap::{ext4_bmap_bit_clr, ext4_bmap_bit_find_clr, ext4_bmap_bit_set, ext4_bmap_is_bit_set},
    block_group::Ext4BlockGroup,
    error::Errno,
    ext4fs::Ext4FileSystem,
    EXT4_BG_INODE_UNINIT,
};

impl Ext4FileSystem {
    fn load_inode_bitmap_for_allocation(
        &self,
        bgid: u32,
        bg: &mut Ext4BlockGroup,
    ) -> Result<(usize, alloc::vec::Vec<u8>), isize> {
        let inode_bitmap_block = bg.get_inode_bitmap_block(&self.superblock) as usize;
        let mut bitmap = self.read_metadata_block(inode_bitmap_block);
        super::counters::inc_counter!(super::counters::INODE_BITMAP_READ);

        if bg.has_flag(EXT4_BG_INODE_UNINIT) {
            if bgid == 0 {
                return Err(Errno::EIO as isize);
            }
            bitmap.fill(0);
            let valid_inodes = self.superblock.get_inodes_in_group_cnt(bgid);
            let bitmap_bits = (bitmap.len() * 8) as u32;
            if valid_inodes > bitmap_bits {
                return Err(Errno::EIO as isize);
            }
            for bit in valid_inodes..bitmap_bits {
                ext4_bmap_bit_set(&mut bitmap, bit);
            }
            bg.clear_flag(EXT4_BG_INODE_UNINIT);
        }

        Ok((inode_bitmap_block, bitmap))
    }

    fn zero_allocated_inode_slot(&self, bg: &Ext4BlockGroup, idx_in_bg: u32) -> Result<(), isize> {
        let inode_size = self.superblock.inode_size() as usize;
        let inode_pos = bg.get_inode_table_blk_num() as usize * self.block_size
            + idx_in_bg as usize * inode_size;
        let block_id = inode_pos / self.block_size;
        let offset = inode_pos % self.block_size;
        if offset + inode_size > self.block_size {
            return Err(Errno::EIO as isize);
        }
        self.with_metadata_block_mut(block_id, |data| {
            data[offset..offset + inode_size].fill(0);
        });
        super::counters::inc_counter!(super::counters::INODE_TABLE_READ);
        super::counters::inc_counter!(super::counters::INODE_TABLE_WRITE);
        Ok(())
    }

    /// Allocate one inode and update bitmap, group descriptor and superblock.
    pub fn ialloc_alloc_inode(&self, is_dir: bool) -> Result<u32, isize> {
        let bg_count = self.superblock.block_group_count();

        for bgid in 0..bg_count {
            let mut bg = self.load_block_group_cached(&self.superblock, bgid as usize);
            let free_inodes = bg.get_free_inodes_count();
            if free_inodes == 0 {
                continue;
            }

            let was_uninit = bg.has_flag(EXT4_BG_INODE_UNINIT);
            let (inode_bitmap_block, mut bitmap) =
                self.load_inode_bitmap_for_allocation(bgid, &mut bg)?;
            let inodes_in_bg = self.superblock.get_inodes_in_group_cnt(bgid);
            let mut idx_in_bg = 0;
            if !ext4_bmap_bit_find_clr(&bitmap, 0, inodes_in_bg, &mut idx_in_bg) {
                log::error!(
                    "ext4 inode bitmap/count mismatch: bg={} free_inodes={}",
                    bgid,
                    free_inodes
                );
                continue;
            }

            self.zero_allocated_inode_slot(&bg, idx_in_bg)?;
            ext4_bmap_bit_set(&mut bitmap, idx_in_bg);
            bg.set_block_group_ialloc_bitmap_csum(&self.superblock, &bitmap);
            self.store_metadata_block_dirty(inode_bitmap_block, &bitmap);
            super::counters::inc_counter!(super::counters::INODE_BITMAP_WRITE);

            bg.set_free_inodes_count(&self.superblock, free_inodes - 1);
            if is_dir {
                let used_dirs = bg.get_used_dirs_count(&self.superblock) + 1;
                bg.set_used_dirs_count(&self.superblock, used_dirs);
            }

            let first_unused = if was_uninit {
                0
            } else {
                inodes_in_bg - bg.get_itable_unused(&self.superblock)
            };
            if idx_in_bg >= first_unused {
                bg.set_itable_unused(&self.superblock, inodes_in_bg - idx_in_bg - 1);
            }

            let mut super_block = self.current_superblock();
            super_block.decrease_free_inodes_count();
            self.defer_bg_write(&bg, bgid, &super_block);
            self.defer_superblock_write(&super_block);

            let inode_num = bgid * self.superblock.inodes_per_group() + idx_in_bg + 1;
            log::debug!(
                "[ALLOC_TRACE] Ino: {}, is_dir: {}, caller: generic_open",
                inode_num,
                is_dir
            );
            return Ok(inode_num);
        }

        println!("[kernel ialloc] alloc inode failed");
        Err(Errno::ENOSPC as isize)
    }

    pub fn ialloc_free_inode(&self, index: u32, is_dir: bool) {
        let bgid = self.get_bgid_of_inode(index);
        let mut super_block = self.current_superblock();
        let mut bg = self.load_block_group_cached(&self.superblock, bgid as usize);
        let inode_bitmap_block = bg.get_inode_bitmap_block(&self.superblock) as usize;
        let mut bitmap = self.read_metadata_block(inode_bitmap_block);
        super::counters::inc_counter!(super::counters::INODE_BITMAP_READ);

        let index_in_group = self.inode_to_bgidx(index);
        if !ext4_bmap_is_bit_set(&bitmap, index_in_group) {
            log::warn!("ignoring duplicate ext4 inode free: ino={}", index);
            return;
        }

        let mut inode_ref = self.get_inode_ref(index);
        inode_ref
            .inode
            .set_dtime((crate::timer::current_time_safe() as u32).max(1));
        self.write_back_inode(&mut inode_ref);

        ext4_bmap_bit_clr(&mut bitmap, index_in_group);
        bg.set_block_group_ialloc_bitmap_csum(&self.superblock, &bitmap);
        self.store_metadata_block_dirty(inode_bitmap_block, &bitmap);
        super::counters::inc_counter!(super::counters::INODE_BITMAP_WRITE);

        bg.set_free_inodes_count(
            &self.superblock,
            bg.get_free_inodes_count().saturating_add(1),
        );
        if is_dir {
            bg.set_used_dirs_count(
                &self.superblock,
                bg.get_used_dirs_count(&self.superblock).saturating_sub(1),
            );
        }
        super_block.increase_free_inodes_count();
        self.defer_bg_write(&bg, bgid, &super_block);
        self.defer_superblock_write(&super_block);
    }
}
