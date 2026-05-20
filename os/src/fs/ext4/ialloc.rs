use crate::fs::{
    ext4::{block_group::Ext4BlockGroup, BLOCK_SIZE},
};
use alloc::vec;

use super::{
    bitmap::{ext4_bmap_bit_clr, ext4_bmap_bit_find_clr, ext4_bmap_bit_set},
    error::Errno,
    ext4fs::Ext4FileSystem,
};

impl Ext4FileSystem {
    /// 分配inode号
    /// # 参数
    /// + is_dir: 是否是文件夹
    /// # 返回值
    /// + 新的inode号
    pub fn ialloc_alloc_inode(&self, is_dir: bool) -> Result<u32, isize> {
        let mut bgid = 0;
        let bg_count = self.superblock.block_group_count();
        let mut super_block = self.superblock;

        while bgid <= bg_count {
            if bgid == bg_count {
                bgid = 0;
                continue;
            }

            // 获取块组
            let mut bg = self.load_block_group_cached(&super_block, bgid as usize);

            let mut free_inodes = bg.get_free_inodes_count();

            if free_inodes > 0 {
                let inode_bitmap_block = bg.get_inode_bitmap_block(&super_block);

                let mut raw_data = self.read_metadata_block(inode_bitmap_block as usize);
                super::counters::inc_counter!(super::counters::INODE_BITMAP_READ);

                let inodes_in_bg = super_block.get_inodes_in_group_cnt(bgid);

                let bitmap_data = &mut raw_data[..];

                let mut idx_in_bg = 0;

                ext4_bmap_bit_find_clr(bitmap_data, 0, inodes_in_bg, &mut idx_in_bg);
                ext4_bmap_bit_set(bitmap_data, idx_in_bg);

                // update bitmap in disk
                // 此处因为是直接进行块单位的写入，所以不需要考虑对齐
                // log::warn!("[WRITE_CALLER] ialloc_alloc_inode: write inode_bitmap block={}, idx_in_bg={}, new_ino={}",
                //     inode_bitmap_block, idx_in_bg, bgid * super_block.inodes_per_group() + (idx_in_bg + 1));
                self.store_metadata_block_dirty(inode_bitmap_block as usize, bitmap_data);
                super::counters::inc_counter!(super::counters::INODE_BITMAP_WRITE);

                bg.set_block_group_ialloc_bitmap_csum(&super_block, bitmap_data);

                // 修改文件系统计数器
                free_inodes -= 1;
                bg.set_free_inodes_count(&super_block, free_inodes);

                /* Increment used directories counter */
                if is_dir {
                    let used_dirs = bg.get_used_dirs_count(&super_block) + 1;
                    bg.set_used_dirs_count(&super_block, used_dirs);
                }

                // 减少未使用inode计数
                let mut unused = bg.get_itable_unused(&super_block);
                let free = inodes_in_bg - unused;
                if idx_in_bg >= free {
                    unused = inodes_in_bg - (idx_in_bg + 1);
                    bg.set_itable_unused(&super_block, unused);
                }

                // 同步块组内容和超级块（Phase 5: defer if batch mode）
                self.defer_bg_write(&bg, bgid as u32, &super_block);
                self.defer_superblock_write(&super_block);
                // 看是否写入成功
                let mut test_super_block = vec![0u8; self.block_size];
                self.block_device.read_block(0, &mut test_super_block);

                /* Compute the absolute i-nodex number */
                // 计算inode号
                let inodes_per_group = super_block.inodes_per_group();
                let inode_num = bgid * inodes_per_group + (idx_in_bg + 1);

                log::debug!(
                    "[ALLOC_TRACE] Ino: {}, is_dir: {}, caller: generic_open",
                    inode_num,
                    is_dir
                );
                return Ok(inode_num);
            }

            bgid += 1;
        }

        println!("[kernel ialloc] alloc inode failed");
        return Err(Errno::ENOSPC as isize);
    }

    pub fn ialloc_free_inode(&self, index: u32, is_dir: bool) {
        log::debug!(
            "[ext4:debug] ialloc_free_inode ENTER: index={}, is_dir={}, inodes_per_group={}, fs_ptr={:p}",
            index,
            is_dir,
            self.superblock.inodes_per_group(),
            self as *const _,
        );
        // Compute index of block group
        let bgid = self.get_bgid_of_inode(index);
        let block_device = self.block_device.clone();

        let mut super_block = self.superblock;
        let mut bg = self.load_block_group_cached(&super_block, bgid as usize);

        // Load inode bitmap block
        let inode_bitmap_block = bg.get_inode_bitmap_block(&self.superblock);
        let mut bitmap_data = self.read_metadata_block(inode_bitmap_block as usize);
        super::counters::inc_counter!(super::counters::INODE_BITMAP_READ);

        // Find index within group and clear bit
        let index_in_group = self.inode_to_bgidx(index);
        ext4_bmap_bit_clr(&mut bitmap_data, index_in_group);

        self.store_metadata_block_dirty(inode_bitmap_block as usize, &bitmap_data);
        super::counters::inc_counter!(super::counters::INODE_BITMAP_WRITE);
        bg.set_block_group_ialloc_bitmap_csum(&super_block, &bitmap_data);

        // Update free inodes count in block group
        let free_inodes = bg.get_free_inodes_count() + 1;
        bg.set_free_inodes_count(&self.superblock, free_inodes);

        // If inode was a directory, decrement the used directories count
        if is_dir {
            let used_dirs = bg.get_used_dirs_count(&self.superblock) - 1;
            bg.set_used_dirs_count(&self.superblock, used_dirs);
        }

        self.defer_bg_write(&bg, bgid as u32, &super_block);

        super_block.increase_free_inodes_count();
        self.defer_superblock_write(&super_block);
    }
}
