use super::*;
use crate::fs::DiskInodeType;
use crate::timer::get_time_sec;
use alloc::vec;
use alloc::vec::Vec;
use block_group::Block;
use ext4fs::Ext4FileSystem;
use path::path_check;
use spin::RwLock;

pub struct Ext4FileContent {
    /// The size of the file.
    pub size: u32,
    /// The block list.
    block_list: Vec<u32>,
    /// The inode number.
    inode: u32,
}

impl Ext4FileContent {
    pub fn new(size: u32, block_list: Vec<u32>, inode: u32) -> Self {
        Self {
            size,
            block_list,
            inode,
        }
    }

    pub fn get_block_list(&self) -> &Vec<u32> {
        &self.block_list
    }
}

use core::cmp::min;

#[allow(unused)]
pub struct FileAttr {
    /// Inode number
    pub ino: u64,
    /// Size in bytes
    pub size: u64,
    /// Size in blocks
    pub blocks: u64,
    /// Time of last access
    pub atime: u32,
    /// Time of last modification
    pub mtime: u32,
    /// Time of last change
    pub ctime: u32,
    /// Time of creation (macOS only)
    pub crtime: u32,
    /// Time of last status change
    pub chgtime: u32,
    /// Backup time (macOS only)
    pub bkuptime: u32,
    /// Kind of file (directory, file, pipe, etc)
    pub kind: InodeFileType,
    /// Permissions
    pub perm: InodePerm,
    /// Number of hard links
    pub nlink: u32,
    /// User id
    pub uid: u32,
    /// Group id
    pub gid: u32,
    /// Rdev
    pub rdev: u32,
    /// Block size
    pub blksize: u32,
    /// Flags (macOS only, see chflags(2))
    pub flags: u32,
}

impl Default for FileAttr {
    fn default() -> Self {
        FileAttr {
            ino: 0,
            size: 0,
            blocks: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
            crtime: 0,
            chgtime: 0,
            bkuptime: 0,
            kind: InodeFileType::S_IFREG,
            perm: InodePerm::S_IREAD | InodePerm::S_IWRITE | InodePerm::S_IEXEC,
            nlink: 0,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 0,
            flags: 0,
        }
    }
}

#[allow(unused)]
impl FileAttr {
    pub fn from_inode_ref(inode_ref: &Ext4InodeRef, block_size: usize) -> FileAttr {
        let inode_num = inode_ref.inode_num;
        let inode = inode_ref.inode;
        FileAttr {
            ino: inode_num as u64,
            size: inode.size(),
            blocks: inode.blocks_count(),
            atime: inode.atime(),
            mtime: inode.mtime(),
            ctime: inode.ctime(),
            crtime: inode.i_crtime(),
            // TODO(ext4-inode-timestamps): Implement `chgtime` (change time)
            // and `bkuptime` (backup time) fields. Exit condition: both fields
            // are populated from the on-disk inode or set to meaningful values.
            chgtime: 0,
            bkuptime: 0,
            kind: inode.file_type(),
            perm: inode.file_perm(), // Extract permission bits
            nlink: inode.links_count() as u32,
            uid: inode.uid() as u32,
            gid: inode.gid() as u32,
            rdev: inode.faddr(),
            blksize: block_size as u32,
            flags: inode.flags(),
        }
    }
}

// #ifdef __i386__
// struct stat {
// 	unsigned long  st_dev;
// 	unsigned long  st_ino;
// 	unsigned short st_mode;
// 	unsigned short st_nlink;
// 	unsigned short st_uid;
// 	unsigned short st_gid;
// 	unsigned long  st_rdev;
// 	unsigned long  st_size;
// 	unsigned long  st_blksize;
// 	unsigned long  st_blocks;
// 	unsigned long  st_atime;
// 	unsigned long  st_atime_nsec;
// 	unsigned long  st_mtime;
// 	unsigned long  st_mtime_nsec;
// 	unsigned long  st_ctime;
// 	unsigned long  st_ctime_nsec;
// 	unsigned long  __unused4;
// 	unsigned long  __unused5;
// };

#[repr(C)]
pub struct LinuxStat {
    st_dev: u32,        // ID of device containing file
    st_ino: u32,        // Inode number
    st_mode: u16,       // File type and mode
    st_nlink: u16,      // Number of hard links
    st_uid: u16,        // User ID of owner
    st_gid: u16,        // Group ID of owner
    st_rdev: u32,       // Device ID (if special file)
    st_size: u32,       // Total size, in bytes
    st_blksize: u32,    // Block size for filesystem I/O
    st_blocks: u32,     // Number of 512B blocks allocated
    st_atime: u32,      // Time of last access
    st_atime_nsec: u32, // Nanoseconds part of last access time
    st_mtime: u32,      // Time of last modification
    st_mtime_nsec: u32, // Nanoseconds part of last modification time
    st_ctime: u32,      // Time of last status change
    st_ctime_nsec: u32, // Nanoseconds part of last status change time
    __unused4: u32,     // Unused field
    __unused5: u32,     // Unused field
}

impl LinuxStat {
    pub fn from_inode_ref(inode_ref: &Ext4InodeRef) -> LinuxStat {
        let inode_num = inode_ref.inode_num;
        let inode = &inode_ref.inode;

        LinuxStat {
            st_dev: 0,
            st_ino: inode_num,
            st_mode: inode.mode,
            st_nlink: inode.links_count(),
            st_uid: inode.uid(),
            st_gid: inode.gid(),
            st_rdev: 0,
            st_size: inode.size() as u32,
            st_blksize: 4096, // 假设块大小为4096字节
            st_blocks: inode.blocks_count() as u32,
            st_atime: inode.atime(),
            st_atime_nsec: 0,
            st_mtime: inode.mtime(),
            st_mtime_nsec: 0,
            st_ctime: inode.ctime(),
            st_ctime_nsec: 0,
            __unused4: 0,
            __unused5: 0,
        }
    }
}

impl Ext4FileSystem {
    /// Link a child inode to a parent directory
    ///
    /// Params:
    /// parent: &mut Ext4InodeRef - parent directory inode reference
    /// child: &mut Ext4InodeRef - child inode reference
    /// name: &str - name of the child inode
    ///
    /// Returns:
    /// `Result<usize>` - status of the operation
    pub fn link(
        &self,
        parent: &mut Ext4InodeRef,
        child: &mut Ext4InodeRef,
        name: &str,
    ) -> Result<usize, isize> {
        // Add a directory entry in the parent directory pointing to the child inode

        // at this point should insert to existing block
        self.dir_add_entry(parent, child, name)?;
        self.write_back_inode_without_csum(parent);

        // If this is the first link. add '.' and '..' entries
        if child.inode.is_dir() {
            // let child_ref = child.clone();
            let new_child_ref = Ext4InodeRef {
                inode_num: child.inode_num,
                inode: child.inode,
            };

            // at this point child need a new block
            self.dir_add_entry(child, &new_child_ref, ".")?;

            // .. should point to parent, not child
            let parent_ref = Ext4InodeRef {
                inode_num: parent.inode_num,
                inode: parent.inode,
            };
            self.dir_add_entry(child, &parent_ref, "..")?;

            child.inode.set_links_count(2);
            let link_cnt = parent.inode.links_count() + 1;
            parent.inode.set_links_count(link_cnt);

            return Ok(EOK);
        }

        // Increment the link count of the child inode
        let link_cnt = child.inode.links_count() + 1;
        child.inode.set_links_count(link_cnt);

        Ok(EOK)
    }

    /// link() 但不 flush parent inode — caller 负责最后统一 flush
    pub fn link_no_parent_flush(
        &self,
        parent: &mut Ext4InodeRef,
        child: &mut Ext4InodeRef,
        name: &str,
    ) -> Result<usize, isize> {
        self.dir_add_entry(parent, child, name)?;
        // skip write_back_inode_without_csum(parent)

        if child.inode.is_dir() {
            let new_child_ref = Ext4InodeRef {
                inode_num: child.inode_num,
                inode: child.inode,
            };
            self.dir_add_entry(child, &new_child_ref, ".")?;
            let parent_ref = Ext4InodeRef {
                inode_num: parent.inode_num,
                inode: parent.inode,
            };
            self.dir_add_entry(child, &parent_ref, "..")?;
            child.inode.set_links_count(2);
            let link_cnt = parent.inode.links_count() + 1;
            parent.inode.set_links_count(link_cnt);
            return Ok(EOK);
        }

        let link_cnt = child.inode.links_count() + 1;
        child.inode.set_links_count(link_cnt);
        Ok(EOK)
    }

    /// 创建一个新inode并将其链接到其父目录
    /// # 参数
    /// + parent: u32 - 父目录的inode号
    /// + name: &str - 新文件的名称
    /// + mode: u16 - 文件模式
    ///
    /// # 返回值:
    /// + `Result<Ext4InodeRef>` - 新文件的inode
    pub fn create(&self, parent: u32, name: &str, inode_mode: u16, uid: u16, gid: u16) -> Result<Ext4InodeRef, isize> {
        let mut parent_inode_ref = self.get_inode_ref(parent);
        let init_child_ref = self.create_inode(inode_mode, uid, gid)?;
        let mut child_mut = init_child_ref.clone();
        self.link_no_parent_flush(&mut parent_inode_ref, &mut child_mut, name)?;
        self.write_back_inode(&mut parent_inode_ref);
        self.write_back_inode(&mut child_mut);
        Ok(child_mut)
    }

    /// 创建 fast symlink（target ≤ 60 字节，存入 i_block 而非分配 data block）。
    ///
    /// Phase 3 优化：避免 create() 的先写空 inode 再读回再写 target 的冗余路径。
    /// 一次初始化 child inode，一次 write_back_inode，一次 write parent。
    pub fn create_fast_symlink(
        &self,
        parent: u32,
        name: &str,
        target: &[u8],
        uid: u16,
        gid: u16,
    ) -> Result<Ext4InodeRef, isize> {
        assert!(target.len() <= 60, "create_fast_symlink: target too long for fast symlink");

        // 1. Allocate inode number
        let ino = self.ialloc_alloc_inode(false)?;

        // 2. Initialize child inode with all fields + target in i_block
        let now = crate::timer::current_time_safe() as u32;
        let mut inode = Ext4Inode::default();
        inode.set_mode(InodeFileType::S_IFLNK.bits() | 0o777);
        inode.set_uid(uid);
        inode.set_gid(gid);
        inode.set_size(target.len() as u64);
        inode.set_atime(now);
        inode.set_mtime(now);
        inode.set_ctime(now);
        // links_count is set by link_no_parent_flush (increments from 0 to 1)
        // Initialize extra inode size for metadata_csum
        let inode_size = self.superblock.inode_size();
        if inode_size > EXT4_GOOD_OLD_INODE_SIZE {
            inode.set_i_extra_isize(self.superblock.extra_size());
        }
        // No EXT4_INODE_FLAG_EXTENTS — fast symlink uses i_block directly
        let block_bytes = inode.block_mut_as_bytes();
        block_bytes[..target.len()].copy_from_slice(target);
        block_bytes[target.len()..60].fill(0);

        let child_ref = Ext4InodeRef { inode_num: ino, inode };

        // 3. Add directory entry and link (no parent flush — done in step 4)
        let mut parent_ref = self.get_inode_ref(parent);
        let mut child_mut = child_ref.clone();
        self.link_no_parent_flush(&mut parent_ref, &mut child_mut, name)?;
        super::counters::inc_counter!(super::counters::SYMLINK_DIR_BLOCK_WRITE_COUNT);

        // 4. Flush parent and child — use child_mut which has updated links_count
        self.write_back_inode(&mut parent_ref);
        super::counters::inc_counter!(super::counters::SYMLINK_PARENT_INODE_WRITE_COUNT);

        self.write_back_inode(&mut child_mut);
        super::counters::inc_counter!(super::counters::SYMLINK_INODE_WRITE_COUNT);

        Ok(child_mut)
    }

    /// 创建inode
    /// # 参数
    /// + inode_mode: inode类型
    /// # 返回值
    /// + 新inode
    pub fn create_inode(&self, inode_mode: u16, uid: u16, gid: u16) -> Result<Ext4InodeRef, isize> {
        // 匹配新inode的文件类型
        let inode_file_type_bits = inode_mode & EXT4_INODE_MODE_TYPE_MASK;
        // println!(
        //     "[kernel create_inode] inode_mode {:?}, {:?}",
        //     inode_mode,
        //     InodeFileType::from_bits(inode_file_type_bits)
        // );
        let inode_file_type = match InodeFileType::from_bits(inode_file_type_bits) {
            Some(file_type) => file_type,
            None => InodeFileType::S_IFREG,
        };
        // println!("[kernel create_inode] {:?}", inode_file_type);

        // 判断是否是文件夹
        let is_dir = inode_file_type == InodeFileType::S_IFDIR;

        // 分配inode
        let inode_num = self.alloc_inode(is_dir);
        if let Err(e) = inode_num {
            return Err(e);
        }

        // 初始化inode
        let mut inode = Ext4Inode::default();

        // 调用者只传文件类型时沿用旧默认权限；传入权限位时保留精确 mode。
        let permission_bits = inode_mode & 0o7777;
        let final_mode = if permission_bits == 0 {
            inode_mode | 0o777
        } else {
            inode_mode
        };
        inode.set_mode(final_mode);
        inode.set_uid(uid);
        inode.set_gid(gid);
        inode.set_uid(uid);
        inode.set_gid(gid);

        // set extra size
        let inode_size = self.superblock.inode_size();
        let extra_size = self.superblock.extra_size();
        if inode_size > EXT4_GOOD_OLD_INODE_SIZE {
            let extra_size = extra_size;
            inode.set_i_extra_isize(extra_size);
        }

        // set extent — only for regular files and directories
        // symlinks use i_block directly (fast) or data blocks (long);
        // device files / fifos / sockets don't need extents
        let needs_extents = inode_file_type == InodeFileType::S_IFREG
            || inode_file_type == InodeFileType::S_IFDIR;
        if needs_extents {
            inode.set_flags(EXT4_INODE_FLAG_EXTENTS as u32);
            inode.extent_tree_init();
        }

        let inode_ref = Ext4InodeRef {
            inode_num: inode_num.unwrap(),
            inode,
        };

        Ok(inode_ref)
    }

    /// create a new inode and link it to the parent directory
    ///
    /// Params:
    /// parent: u32 - inode number of the parent directory
    /// name: &str - name of the new file
    /// mode: u16 - file mode
    /// uid: u32 - user id
    /// gid: u32 - group id
    ///
    /// Returns:
    pub fn create_with_attr(
        &self,
        parent: u32,
        name: &str,
        inode_mode: u16,
        uid: u16,
        gid: u16,
    ) -> Result<Ext4InodeRef, isize> {
        let mut parent_inode_ref = self.get_inode_ref(parent);
        let mut init_child_ref = self.create_inode(inode_mode, uid, gid)?;
        let mut child_mut = init_child_ref.clone();
        self.link_no_parent_flush(&mut parent_inode_ref, &mut child_mut, name)?;
        self.write_back_inode(&mut parent_inode_ref);
        self.write_back_inode(&mut child_mut);
        Ok(child_mut)
    }

    /// 从指定文件的某个偏移位置开始读取数据
    /// # 参数
    /// + inode: u32 - 文件的inode号
    /// + offset: usize - offset from where to read
    /// + read_buf: &mut [u8] - 存储读取的数据的buffer
    /// # 返回值
    /// `Result<usize>`：读取的字节数
    pub fn read_at(&self, inode: u32, offset: usize, read_buf: &mut [u8]) -> Result<usize, isize> {
        // 缓冲区为空，返回 0
        let mut read_buf_len = read_buf.len();
        if read_buf_len == 0 {
            return Ok(0);
        }

        // 获取ext4inoderef对象
        let inode_ref = self.get_inode_ref(inode);

        // 获取文件大小
        let file_size = inode_ref.inode.size();

        // Fast symlink: target stored in i_block (no data blocks, no extents)
        {
            let is_symlink = inode_ref.inode.get_file_type() == DiskInodeType::Link;
            let uses_extents = (inode_ref.inode.flags()
                & crate::fs::ext4::EXT4_INODE_FLAG_EXTENTS as u32) != 0;
            if is_symlink && !uses_extents && file_size <= 60 {
                let size = file_size as usize;
                let to_read = core::cmp::min(size, read_buf.len());
                let block_bytes = inode_ref.inode.block_as_bytes();
                read_buf[..to_read].copy_from_slice(&block_bytes[..to_read]);
                return Ok(to_read);
            }
        }

        // 如果偏移量大于文件大小，返回 0
        if offset >= file_size as usize {
            return Ok(0);
        }

        // 如果 offset + read_buf_len 大于 file_size，调整读取大小
        if offset + read_buf_len > file_size as usize {
            read_buf_len = file_size as usize - offset;
        }

        // adjust the read buffer size if the read buffer size is greater than the file size
        // 这步是不是和上一步重了？
        let size_to_read = min(read_buf_len, file_size as usize - offset);

        // 计算起始块以及未对齐大小
        let iblock_start = offset / self.block_size;
        let iblock_last = (offset + size_to_read + self.block_size - 1) / self.block_size; // round up to include the last partial block
        let unaligned_start_offset = offset % self.block_size;

        // Buffer to keep track of read bytes
        let mut cursor = 0;
        let mut total_bytes_read = 0;
        let mut iblock = iblock_start;

        // Unaligned read at the beginning
        // 处理起始块未对齐的情况
        if unaligned_start_offset > 0 {
            let adjust_read_size = min(self.block_size - unaligned_start_offset, size_to_read);

            // 获取逻辑块号对应的物理块号
            match self.get_pblock_idx(&inode_ref, iblock as u32) {
                Ok(pblock_idx) => {
                    let mut data = vec![0u8; self.block_size];
                    self.block_device.read_block(pblock_idx as usize, &mut data);
                    read_buf[cursor..cursor + adjust_read_size].copy_from_slice(
                        &data[unaligned_start_offset..unaligned_start_offset + adjust_read_size],
                    );
                }
                Err(_) => {
                    // sparse hole: fill with zeros
                    read_buf[cursor..cursor + adjust_read_size].fill(0);
                }
            }

            // 更新 cursor 以及 total_bytes_read
            cursor += adjust_read_size;
            total_bytes_read += adjust_read_size;
            iblock += 1;
        }

        // Continue with full block reads
        // 继续处理整个的块
        while total_bytes_read < size_to_read {
            let read_length = core::cmp::min(self.block_size, size_to_read - total_bytes_read);

            // 获取逻辑块号对应的物理块号
            match self.get_pblock_idx(&inode_ref, iblock as u32) {
                Ok(pblock_idx) => {
                    let mut data = vec![0u8; self.block_size];
                    self.block_device.read_block(pblock_idx as usize, &mut data);
                    read_buf[cursor..cursor + read_length].copy_from_slice(&data[..read_length]);
                }
                Err(_) => {
                    // sparse hole: fill with zeros
                    read_buf[cursor..cursor + read_length].fill(0);
                }
            }

            // 更新 cursor 以及 total_bytes_read
            cursor += read_length;
            total_bytes_read += read_length;
            iblock += 1;
        }

        Ok(min(total_bytes_read, size_to_read))
    }

    /// 将数据按指定的offset写入到一个文件中
    ///
    /// 参数:
    /// inode: u32 - 文件的inode号
    /// offset: usize - 指定开始写的位置的偏移量
    /// write_buf: &[u8] - 要写入的buffer
    ///
    /// Returns:
    /// `Result<usize>` - 写入的字节数
    pub fn write_at(&self, inode: u32, offset: usize, write_buf: &[u8]) -> Result<usize, isize> {
        // write_buf为空, 返回0
        let write_buf_len = write_buf.len();
        if write_buf_len == 0 {
            return Ok(0);
        }

        // get the inode reference
        let mut inode_ref = self.get_inode_ref(inode);

        // Get the file size
        let file_size = inode_ref.inode.size();

        // Calculate the start and end block index
        let iblock_start = offset / self.block_size;
        let iblock_last = (offset + write_buf_len + self.block_size - 1) / self.block_size; // round up to include the last partial block

        // start block index
        let mut iblk_idx = iblock_start;

        // Calculate the unaligned size
        let unaligned = offset % self.block_size;

        // Buffer to keep track of written bytes
        let mut written = 0;

        // Start bgid
        let mut start_bgid = 1;

        // Unaligned write
        if unaligned > 0 {
            let len = min(write_buf_len, self.block_size - unaligned);
            // Get the physical block id, allocate a new block if it's a hole
            let pblock_idx = match self.get_pblock_idx(&inode_ref, iblk_idx as u32) {
                Ok(p) => p,
                Err(_) => {
                    self.insert_inode_pblk_from(&mut inode_ref, iblk_idx as u32, &mut start_bgid)?
                }
            };

            let mut block = Block::load_offset(
                self.block_device.clone(),
                pblock_idx as usize * self.block_size,
                self.block_size,
            );

            block.write_offset(unaligned, &write_buf[..len], len);
            block.sync_blk_to_disk(self.block_device.clone());
            super::counters::inc_counter!(super::counters::DATA_BLOCK_WRITE);
            drop(block);

            written += len;
            iblk_idx += 1;
        }

        // Aligned write
        let mut fblock_start = 0;
        let mut fblock_count = 0;

        while written < write_buf_len {
            while iblk_idx < iblock_last && written < write_buf_len {
                // Get the physical block id, allocate a new block if it's a hole
                let pblock_idx = match self.get_pblock_idx(&inode_ref, iblk_idx as u32) {
                    Ok(p) => p,
                    Err(_) => self.insert_inode_pblk_from(
                        &mut inode_ref,
                        iblk_idx as u32,
                        &mut start_bgid,
                    )?,
                };
                if fblock_start == 0 {
                    fblock_start = pblock_idx;
                }

                // Check if the block is contiguous
                if fblock_start + fblock_count != pblock_idx {
                    break;
                }

                fblock_count += 1;
                iblk_idx += 1;
            }

            // Write contiguous blocks at once
            let len = min(
                fblock_count as usize * self.block_size,
                write_buf_len - written,
            );

            for i in 0..fblock_count {
                let block_offset =
                    fblock_start as usize * self.block_size + i as usize * self.block_size;
                let mut block = Block::load_offset(self.block_device.clone(), block_offset, self.block_size);
                let write_size = min(self.block_size, write_buf_len - written);
                block.write_offset(0, &write_buf[written..written + write_size], write_size);
                block.sync_blk_to_disk(self.block_device.clone());
                super::counters::inc_counter!(super::counters::DATA_BLOCK_WRITE);
                drop(block);
                written += write_size;
            }

            fblock_start = 0;
            fblock_count = 0;
        }

        // Final unaligned write if any
        if written < write_buf_len {
            let len = write_buf_len - written;
            // Get the physical block id, allocate a new block if it's a hole
            let pblock_idx = match self.get_pblock_idx(&inode_ref, iblk_idx as u32) {
                Ok(p) => p,
                Err(_) => self.insert_inode_pblk(&mut inode_ref, iblk_idx as u32)?,
            };

            let mut block = Block::load_offset(
                self.block_device.clone(),
                pblock_idx as usize * self.block_size,
                self.block_size,
            );
            block.write_offset(0, &write_buf[written..], len);
            block.sync_blk_to_disk(self.block_device.clone());
            super::counters::inc_counter!(super::counters::DATA_BLOCK_WRITE);
            drop(block);

            written += len;
        }

        // Update file size if necessary
        if offset + write_buf_len > file_size as usize {
            log::trace!("set file size {:x}", offset + write_buf_len);
            inode_ref.inode.set_size((offset + write_buf_len) as u64);

            self.write_back_inode(&mut inode_ref);
        }

        Ok(written)
    }

    /// File remove
    ///
    /// Params:
    /// path: file path start from root
    ///
    /// Returns:
    /// `Result<usize>` - status of the operation
    pub fn file_remove(&self, path: &str) -> Result<usize, isize> {
        // start from root
        let mut parent_inode_num = ROOT_INODE;

        let mut nameoff = 0;
        let child_inode = self.generic_open(path, &mut parent_inode_num, false, 0, &mut nameoff)?;

        let mut child_inode_ref = self.get_inode_ref(child_inode);
        let child_link_cnt = child_inode_ref.inode.links_count();
        if child_link_cnt == 1 {
            self.truncate_inode(&mut child_inode_ref, 0)?;
        }

        // get child name
        let mut is_goal = false;
        let p = &path[nameoff as usize..];
        let len = path_check(p, &mut is_goal);

        // load parent
        let mut parent_inode_ref = self.get_inode_ref(parent_inode_num);

        let r = self.unlink(&mut parent_inode_ref, &mut child_inode_ref, &p[..len])?;

        Ok(EOK)
    }

    /// File truncate
    /// + 参数
    /// inode_ref: &mut Ext4InodeRef - inode reference
    /// new_size: u64 - 文件的新大小
    /// + 返回值
    /// `Result<usize>` - 操作状态
    pub fn truncate_inode(
        &self,
        inode_ref: &mut Ext4InodeRef,
        new_size: u64,
    ) -> Result<usize, isize> {
        log::info!(
            "[debug_truncate] before: ino: {}, mode: {:#o}, size: {}",
            inode_ref.inode_num,
            inode_ref.inode.mode,
            inode_ref.inode.size()
        );
        let old_size = inode_ref.inode.size();

        // 文件扩展或不变：只更新 size 后返回（block 分配由 write 路径按需触发）
        if old_size < new_size {
            inode_ref.inode.set_size(new_size);
            self.write_back_inode(inode_ref);
            return Ok(EOK);
        }

        if old_size == new_size {
            return Ok(EOK);
        }

        // 如果是 Fast Symlink，它没有分配任何数据块，
        // 它的数据全存在 inode.block 数组里，因此不需要释放任何物理块。
        // 直接清零 inode.block 并更新 size 即可。
        let is_symlink = inode_ref.inode.get_file_type() == DiskInodeType::Link;
        let uses_extents =
            (inode_ref.inode.flags() & crate::fs::ext4::EXT4_INODE_FLAG_EXTENTS as u32) != 0;

        if is_symlink && !uses_extents {
            // Fast Symlink 的截断逻辑
            if new_size == 0 {
                // 清零 block 数组
                inode_ref.inode.set_block([0u32; 15]);
            } else {
                // 如果截断到特定大小，清零超出部分
                // Safety: `block_ptr` is derived from `inode_ref.inode.block`,
                // which is a valid `[u32; 15]` array (60 bytes). `new_size` and
                // `old_size` are file sizes, not byte offsets — this zeroes the
                // trailing extent entries in the block array. Bounds: caller
                // guarantees `new_size < old_size <= 15` (number of u32 entries).
                unsafe {
                    let block_ptr = inode_ref.inode.block.as_mut_ptr() as *mut u8;
                    core::ptr::write_bytes(
                        block_ptr.add(new_size as usize),
                        0,
                        old_size as usize - new_size as usize,
                    );
                }
            }
            inode_ref.inode.set_size(new_size);
            self.write_back_inode(inode_ref);
            return Ok(EOK);
        }

        let block_size = self.block_size as u64;
        let new_blocks_cnt = ((new_size + block_size - 1) / block_size) as u32;
        let old_blocks_cnt = ((old_size + block_size - 1) / block_size) as u32;
        let diff_blocks_cnt = old_blocks_cnt.saturating_sub(new_blocks_cnt); // 防止下溢

        if diff_blocks_cnt > 0 {
            self.extent_remove_space(inode_ref, new_blocks_cnt, EXT_MAX_BLOCKS)?;
        }

        inode_ref.inode.set_size(new_size);
        self.write_back_inode(inode_ref);

        log::info!(
            "[debug_truncate] after: ino: {}, mode: {:#o}, size: {}",
            inode_ref.inode_num,
            inode_ref.inode.mode,
            inode_ref.inode.size()
        );
        Ok(EOK)
    }
}

pub struct Ext4FileContentWrapper {
    file_content_inner: RwLock<Ext4FileContent>,
}
