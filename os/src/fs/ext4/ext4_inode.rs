use core::cmp::min;
use core::mem::size_of;
use core::panic;

use crate::fs::inode::{InodeLock, InodeTime};
use crate::fs::DiskInodeType;
use crate::fs::vfs::FileSystem;
use alloc::string::String;
use alloc::vec;
use alloc::{sync::Arc, vec::Vec};
use spin::{Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};

use super::*;
use super::{
    bitmap::{ext4_bmap_bit_find_clr, ext4_bmap_bit_set},
    block_group::{Block, Ext4BlockGroup},
    crc::{ext4_crc32c, EXT4_CRC32_INIT},
    direntry::DirEntryType,
    error::Errno,
    ext4fs::Ext4FileSystem,
    extent::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIndex},
    superblock::Ext4Superblock,
    BlockDevice,
};

bitflags! {
    pub struct InodeFileType: u16 {
        const S_IFIFO = 0x1000;
        const S_IFCHR = 0x2000;
        const S_IFDIR = 0x4000;
        const S_IFBLK = 0x6000;
        const S_IFREG = 0x8000;
        const S_IFSOCK = 0xC000;
        const S_IFLNK = 0xA000;
    }
}

impl InodeFileType {
    pub fn to_dir_entry_type(&self) -> crate::fs::ext4::direntry::DirEntryType {
        use crate::fs::ext4::direntry::DirEntryType;
        if self.contains(InodeFileType::S_IFREG) { DirEntryType::EXT4_DE_REG_FILE }
        else if self.contains(InodeFileType::S_IFDIR) { DirEntryType::EXT4_DE_DIR }
        else if self.contains(InodeFileType::S_IFLNK) { DirEntryType::EXT4_DE_SYMLINK }
        else if self.contains(InodeFileType::S_IFCHR) { DirEntryType::EXT4_DE_CHRDEV }
        else if self.contains(InodeFileType::S_IFBLK) { DirEntryType::EXT4_DE_BLKDEV }
        else if self.contains(InodeFileType::S_IFIFO) { DirEntryType::EXT4_DE_FIFO }
        else if self.contains(InodeFileType::S_IFSOCK) { DirEntryType::EXT4_DE_SOCK }
        else { DirEntryType::EXT4_DE_UNKNOWN }
    }
}

bitflags! {
    pub struct InodePerm: u16 {
        const S_IREAD = 0x0100;
        const S_IWRITE = 0x0080;
        const S_IEXEC = 0x0040;
        const S_ISUID = 0x0800;
        const S_ISGID = 0x0400;
    }
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ext4Inode {
    pub mode: u16,        // 文件类型和权限
    pub uid: u16,         // 所有者用户 ID
    pub size: u32,        // 低 32 位文件大小
    pub atime: u32,       // 最近访问时间
    pub ctime: u32,       // 创建时间
    pub mtime: u32,       // 最近修改时间
    pub dtime: u32,       // 删除时间
    pub gid: u16,         // 所有者组 ID
    pub links_count: u16, // 链接计数
    pub blocks: u32,      // 已分配的块数
    pub flags: u32,       // 文件标志
    pub osd1: u32,        // 操作系统相关的字段1
    pub block: [u32; 15], // 数据块指针
    pub generation: u32,  // 文件版本（NFS）
    pub file_acl: u32,    // 文件 ACL
    pub size_hi: u32,     // 高 32 位文件大小
    pub faddr: u32,       // 已废弃的碎片地址
    pub osd2: Linux2,     // 操作系统相关的字段2

    pub i_extra_isize: u16,  // 额外的 inode 大小
    pub i_checksum_hi: u16,  // 高位校验和（crc32c(uuid+inum+inode) BE）
    pub i_ctime_extra: u32,  // 额外的创建时间（纳秒 << 2 | 纪元）
    pub i_mtime_extra: u32,  // 额外的修改时间（纳秒 << 2 | 纪元）
    pub i_atime_extra: u32,  // 额外的访问时间（纳秒 << 2 | 纪元）
    pub i_crtime: u32,       // 创建时间
    pub i_crtime_extra: u32, // 额外的创建时间（纳秒 << 2 | 纪元）
    pub i_version_hi: u32,   // 高 32 位版本
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Linux2 {
    pub l_i_blocks_high: u16,   // 高 16 位已分配块数
    pub l_i_file_acl_high: u16, // 高 16 位文件 ACL
    pub l_i_uid_high: u16,      // 高 16 位用户 ID
    pub l_i_gid_high: u16,      // 高 16 位组 ID
    pub l_i_checksum_lo: u16,   // 低位校验和
    pub l_i_reserved: u16,      // 保留字段
}

#[allow(unused)]
impl Ext4Inode {
    pub fn root_inode(ext4fs: &Arc<Ext4FileSystem>) -> Arc<Self> {
        // 尝试获取根目录的Inode节点
        let root_inode = ext4fs.get_inode_ref(ROOT_INODE);
        // root_inode.inode.clone()
        todo!()
    }

    pub fn mode(&self) -> u16 {
        self.mode
    }

    pub fn set_mode(&mut self, mode: u16) {
        self.mode = mode;
    }

    pub fn uid(&self) -> u16 {
        self.uid
    }

    pub fn set_uid(&mut self, uid: u16) {
        self.uid = uid;
    }

    pub fn size(&self) -> u64 {
        self.size as u64 | ((self.size_hi as u64) << 32)
    }

    pub fn set_size(&mut self, size: u64) {
        self.size = (size & 0xffffffff) as u32;
        self.size_hi = (size >> 32) as u32;
    }

    pub fn atime(&self) -> u32 {
        self.atime
    }

    pub fn set_atime(&mut self, atime: u32) {
        self.atime = atime;
    }

    pub fn ctime(&self) -> u32 {
        self.ctime
    }

    pub fn set_ctime(&mut self, ctime: u32) {
        self.ctime = ctime;
    }

    pub fn mtime(&self) -> u32 {
        self.mtime
    }

    pub fn set_mtime(&mut self, mtime: u32) {
        self.mtime = mtime;
    }

    pub fn dtime(&self) -> u32 {
        self.dtime
    }

    pub fn set_dtime(&mut self, dtime: u32) {
        self.dtime = dtime;
    }

    pub fn gid(&self) -> u16 {
        self.gid
    }

    pub fn set_gid(&mut self, gid: u16) {
        self.gid = gid;
    }

    pub fn links_count(&self) -> u16 {
        self.links_count
    }

    pub fn set_links_count(&mut self, links_count: u16) {
        self.links_count = links_count;
    }

    pub fn blocks_count(&self) -> u64 {
        let mut blocks = self.blocks as u64;
        if self.osd2.l_i_blocks_high != 0 {
            blocks |= (self.osd2.l_i_blocks_high as u64) << 32;
        }
        blocks
    }

    pub fn set_blocks_count(&mut self, blocks: u64) {
        self.blocks = (blocks & 0xFFFFFFFF) as u32;
        self.osd2.l_i_blocks_high = (blocks >> 32) as u16;
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
    }

    pub fn osd1(&self) -> u32 {
        self.osd1
    }

    pub fn set_osd1(&mut self, osd1: u32) {
        self.osd1 = osd1;
    }

    pub fn block(&self) -> [u32; 15] {
        self.block
    }

    pub fn set_block(&mut self, block: [u32; 15]) {
        self.block = block;
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn set_generation(&mut self, generation: u32) {
        self.generation = generation;
    }

    pub fn file_acl(&self) -> u32 {
        self.file_acl
    }

    pub fn set_file_acl(&mut self, file_acl: u32) {
        self.file_acl = file_acl;
    }

    pub fn size_hi(&self) -> u32 {
        self.size_hi
    }

    pub fn set_size_hi(&mut self, size_hi: u32) {
        self.size_hi = size_hi;
    }

    pub fn faddr(&self) -> u32 {
        self.faddr
    }

    pub fn set_faddr(&mut self, faddr: u32) {
        self.faddr = faddr;
    }

    pub fn osd2(&self) -> Linux2 {
        self.osd2
    }

    pub fn set_osd2(&mut self, osd2: Linux2) {
        self.osd2 = osd2;
    }

    pub fn i_extra_isize(&self) -> u16 {
        self.i_extra_isize
    }

    pub fn set_i_extra_isize(&mut self, i_extra_isize: u16) {
        self.i_extra_isize = i_extra_isize;
    }

    pub fn i_checksum_hi(&self) -> u16 {
        self.i_checksum_hi
    }

    pub fn set_i_checksum_hi(&mut self, i_checksum_hi: u16) {
        self.i_checksum_hi = i_checksum_hi;
    }

    pub fn i_ctime_extra(&self) -> u32 {
        self.i_ctime_extra
    }

    pub fn set_i_ctime_extra(&mut self, i_ctime_extra: u32) {
        self.i_ctime_extra = i_ctime_extra;
    }

    pub fn i_mtime_extra(&self) -> u32 {
        self.i_mtime_extra
    }

    pub fn set_i_mtime_extra(&mut self, i_mtime_extra: u32) {
        self.i_mtime_extra = i_mtime_extra;
    }

    pub fn i_atime_extra(&self) -> u32 {
        self.i_atime_extra
    }

    pub fn set_i_atime_extra(&mut self, i_atime_extra: u32) {
        self.i_atime_extra = i_atime_extra;
    }

    pub fn i_crtime(&self) -> u32 {
        self.i_crtime
    }

    pub fn set_i_crtime(&mut self, i_crtime: u32) {
        self.i_crtime = i_crtime;
    }

    pub fn i_crtime_extra(&self) -> u32 {
        self.i_crtime_extra
    }

    pub fn set_i_crtime_extra(&mut self, i_crtime_extra: u32) {
        self.i_crtime_extra = i_crtime_extra;
    }

    pub fn i_version_hi(&self) -> u32 {
        self.i_version_hi
    }

    pub fn set_i_version_hi(&mut self, i_version_hi: u32) {
        self.i_version_hi = i_version_hi;
    }
}

impl Ext4Inode {
    pub fn file_type(&self) -> InodeFileType {
        InodeFileType::from_bits_truncate(self.mode & EXT4_INODE_MODE_TYPE_MASK)
    }

    pub fn file_perm(&self) -> InodePerm {
        InodePerm::from_bits_truncate(self.mode & EXT4_INODE_MODE_PERM_MASK)
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == InodeFileType::S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.file_type() == InodeFileType::S_IFREG
    }

    pub fn is_link(&self) -> bool {
        self.file_type() == InodeFileType::S_IFLNK
    }

    pub fn get_file_type(&self) -> crate::fs::DiskInodeType {
        let file_type = self.file_type();
        match file_type {
            InodeFileType::S_IFDIR => crate::fs::DiskInodeType::Directory,
            InodeFileType::S_IFREG => crate::fs::DiskInodeType::File,
            InodeFileType::S_IFBLK => crate::fs::DiskInodeType::Block,
            InodeFileType::S_IFCHR => crate::fs::DiskInodeType::Character,
            InodeFileType::S_IFIFO => crate::fs::DiskInodeType::FIFO,
            InodeFileType::S_IFSOCK => crate::fs::DiskInodeType::Socket,
            InodeFileType::S_IFLNK => crate::fs::DiskInodeType::Link,
            _ => panic!("Unknown disk type: mode={:#x}", self.mode),
        }
    }

    pub fn can_read(&self) -> bool {
        self.file_perm().contains(InodePerm::S_IREAD)
    }

    pub fn can_write(&self) -> bool {
        self.file_perm().contains(InodePerm::S_IWRITE)
    }

    pub fn can_exec(&self) -> bool {
        self.file_perm().contains(InodePerm::S_IEXEC)
    }

    pub fn set_file_type(&mut self, kind: InodeFileType) {
        self.mode |= kind.bits();
    }

    pub fn set_file_perm(&mut self, perm: InodePerm) {
        self.mode |= perm.bits();
    }
}

#[derive(Clone, Debug)]
/// Ext4Inode封装对象
/// + 含有inode号和inode
pub struct Ext4InodeRef {
    pub inode_num: u32,
    pub inode: Ext4Inode,
}

impl Ext4InodeRef {
    pub fn set_atime(&mut self, atime: u32) {
        self.inode.set_atime(atime);
    }
    pub fn set_mtime(&mut self, mtime: u32) {
        self.inode.set_mtime(mtime);
    }
    pub fn set_ctime(&mut self, ctime: u32) {
        self.inode.set_ctime(ctime);
    }
}

// ── Phase 4: CachedExt4Inode ──────────────────────────────────────────────
//
// DragonOS 底层是 another_ext4 库（可能有内部缓存），当前内核自己实现 ext4
// 磁盘逻辑，因此需要此层缓存来减少 get_inode_ref 的磁盘 I/O。
//
// 当前策略:
//   - 缓存 inode 数据 + inode table 位置，支持原地修改后写回
//   - write_back_inode 不再绕过缓存直接写盘

/// 带缓存的 ext4 inode，记录其在 inode table 中的位置，支持原地写回
#[derive(Debug, Clone)]
pub struct CachedExt4Inode {
    pub ino: u32,
    pub inode: Ext4Inode,
    /// inode table block 起始位置（字节偏移）
    pub inode_table_block: usize,
    /// inode 在 block 内的偏移
    pub offset_in_block: usize,
    /// 是否有未写回的变更
    pub dirty: bool,
}

impl CachedExt4Inode {
    /// 从 Ext4InodeRef 创建，但需要额外提供 inode table 位置信息
    pub fn from_ref(inode_ref: &Ext4InodeRef, table_block: usize, offset: usize) -> Self {
        Self {
            ino: inode_ref.inode_num,
            inode: inode_ref.inode,
            inode_table_block: table_block,
            offset_in_block: offset,
            dirty: false,
        }
    }

    /// 返回 Ext4InodeRef 快照（不包含位置信息）
    pub fn to_ref(&self) -> Ext4InodeRef {
        Ext4InodeRef {
            inode_num: self.ino,
            inode: self.inode,
        }
    }
}

impl Ext4Inode {
    /// Get the depth of the extent tree from an inode.
    pub fn root_header_depth(&self) -> u16 {
        self.root_extent_header().depth
    }

    pub fn root_extent_header_ref(&self) -> &Ext4ExtentHeader {
        let header_ptr = self.block.as_ptr() as *const Ext4ExtentHeader;
        unsafe { &*header_ptr }
    }

    pub fn root_extent_header(&self) -> Ext4ExtentHeader {
        let header_ptr = self.block.as_ptr() as *const Ext4ExtentHeader;
        unsafe { *header_ptr }
    }

    pub fn root_extent_header_mut(&mut self) -> &mut Ext4ExtentHeader {
        let header_ptr = self.block.as_mut_ptr() as *mut Ext4ExtentHeader;
        unsafe { &mut *header_ptr }
    }

    pub fn root_extent_mut_at(&mut self, pos: usize) -> &mut Ext4Extent {
        let header_ptr = self.block.as_mut_ptr() as *mut Ext4ExtentHeader;
        unsafe { &mut *(header_ptr.add(1) as *mut Ext4Extent).add(pos) }
    }

    pub fn root_extent_ref_at(&mut self, pos: usize) -> &Ext4Extent {
        let header_ptr = self.block.as_ptr() as *const Ext4ExtentHeader;
        unsafe { &*(header_ptr.add(1) as *const Ext4Extent).add(pos) }
    }

    pub fn root_extent_at(&mut self, pos: usize) -> Ext4Extent {
        let header_ptr = self.block.as_ptr() as *const Ext4ExtentHeader;
        unsafe { *(header_ptr.add(1) as *const Ext4Extent).add(pos) }
    }

    pub fn root_first_index_mut(&mut self) -> &mut Ext4ExtentIndex {
        let header_ptr = self.block.as_mut_ptr() as *mut Ext4ExtentHeader;
        unsafe { &mut *(header_ptr.add(1) as *mut Ext4ExtentIndex) }
    }

    pub fn extent_tree_init(&mut self) {
        let header_ptr = self.block.as_mut_ptr() as *mut Ext4ExtentHeader;
        unsafe {
            (*header_ptr).set_magic();
            (*header_ptr).set_entries_count(0);
            (*header_ptr).set_max_entries_count(4);
            (*header_ptr).set_depth(0);
            (*header_ptr).set_generation(0);
        }
    }

    /// View the 15-element block array as &[u8; 60] — used for fast symlink targets
    pub fn block_as_bytes(&self) -> &[u8; 60] {
        unsafe { &*(self.block.as_ptr() as *const [u8; 60]) }
    }

    /// Mutable view of the 15-element block array as &mut [u8; 60] — for writing fast symlink targets
    pub fn block_mut_as_bytes(&mut self) -> &mut [u8; 60] {
        unsafe { &mut *(self.block.as_mut_ptr() as *mut [u8; 60]) }
    }

    fn get_checksum(&self, super_block: &Ext4Superblock) -> u32 {
        let inode_size = super_block.inode_size;
        let mut v: u32 = self.osd2.l_i_checksum_lo as u32;
        if inode_size > 128 {
            v |= (self.i_checksum_hi as u32) << 16;
        }
        v
    }
    #[allow(unused)]
    pub fn set_inode_checksum_value(
        &mut self,
        super_block: &Ext4Superblock,
        inode_id: u32,
        checksum: u32,
    ) {
        let inode_size = super_block.inode_size();

        self.osd2.l_i_checksum_lo = (checksum & 0xffff) as u16;
        if inode_size > 128 {
            self.i_checksum_hi = (checksum >> 16) as u16;
        }
    }
    fn copy_to_slice(&self, slice: &mut [u8]) {
        unsafe {
            let inode_ptr = self as *const Ext4Inode as *const u8;
            let array_ptr = slice.as_ptr() as *mut u8;
            core::ptr::copy_nonoverlapping(inode_ptr, array_ptr, 0x9c);
        }
    }
    #[allow(unused)]
    pub fn get_inode_checksum(&mut self, inode_id: u32, super_block: &Ext4Superblock) -> u32 {
        let inode_size = super_block.inode_size();

        let orig_checksum = self.get_checksum(super_block);
        let mut checksum = 0;

        let ino_index = inode_id;
        let ino_gen = self.generation;

        // Preparation: temporarily set bg checksum to 0
        self.osd2.l_i_checksum_lo = 0;
        self.i_checksum_hi = 0;

        checksum = ext4_crc32c(
            EXT4_CRC32_INIT,
            &super_block.uuid,
            super_block.uuid.len() as u32,
        );
        checksum = ext4_crc32c(checksum, &ino_index.to_le_bytes(), 4);
        checksum = ext4_crc32c(checksum, &ino_gen.to_le_bytes(), 4);

        let mut raw_data = [0u8; 0x100];
        self.copy_to_slice(&mut raw_data);

        // inode checksum
        checksum = ext4_crc32c(checksum, &raw_data, inode_size as u32);

        self.set_inode_checksum_value(super_block, inode_id, checksum);

        if inode_size == 128 {
            checksum &= 0xFFFF;
        }

        checksum
    }

    pub fn set_inode_checksum(&mut self, super_block: &Ext4Superblock, inode_id: u32) {
        let inode_size = super_block.inode_size();
        let checksum = self.get_inode_checksum(inode_id, super_block);

        self.osd2.l_i_checksum_lo = ((checksum << 16) >> 16) as u16;
        if inode_size > 128 {
            self.i_checksum_hi = (checksum >> 16) as u16;
        }
    }

    //
    pub fn sync_inode_to_disk(
        &self,
        block_device: Arc<dyn BlockDevice>,
        inode_pos: usize,
        on_disk_size: usize,
        inode_num: u32,
        block_size: usize,
    ) {
        if inode_num == 3266 {
            log::warn!(
                "[WRITE_TRACE] Writing Ino 3266! Mode: 0o{:o}, Size: {}, FirstBlock: {}",
                self.mode,
                self.size,
                self.block[0]
            );
        }
        // 【关键修改】取 Rust 结构体大小和磁盘 Inode 大小的最小值
        // 即使结构体定义了 156 字节，如果磁盘只给每个 Inode 留了 128 字节，我们也只写前 128 字节
        let write_len = core::cmp::min(core::mem::size_of::<Ext4Inode>(), on_disk_size);

        let data = unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, write_len) };

        let block_id = inode_pos / block_size;
        let offset = inode_pos % block_size;

        log::warn!(
            "[WRITE_CALLER] sync_inode_to_disk: ino={}, block_id={}, offset={}, mode=0o{:o}, size={}",
            inode_num, block_id, offset, self.mode, self.size()
        );

        let mut buf = vec![0u8; block_size];
        block_device.read_block(block_id, &mut buf);
        super::counters::inc_counter!(super::counters::BLOCK_READ_TOTAL);
        super::counters::inc_counter!(super::counters::INODE_TABLE_READ);

        buf[offset..offset + write_len].copy_from_slice(&data);
        block_device.write_block(block_id, &buf);
        super::counters::inc_counter!(super::counters::BLOCK_WRITE_TOTAL);
        super::counters::inc_counter!(super::counters::INODE_TABLE_WRITE);
    }
}

impl Ext4FileSystem {
    pub fn get_bgid_of_inode(&self, inode_num: u32) -> u32 {
        let ipg = self.superblock.inodes_per_group();
        if ipg == 0 {
            log::warn!(
                "[ext4:debug] FATAL: get_bgid_of_inode: inode={}, inodes_per_group=0 !!! \
                 fs_ptr={:p}, sb_ptr={:p}",
                inode_num,
                self as *const _,
                &self.superblock as *const _,
            );
        }
        (inode_num - 1) / ipg
    }

    pub fn inode_to_bgidx(&self, inode_num: u32) -> u32 {
        (inode_num - 1) % self.superblock.inodes_per_group()
    }

    /// 获取Inode的地址
    /// # 参数
    /// + inode_num: Inode号
    /// # 返回值
    /// + inode在块设备上的偏移量
    pub fn inode_disk_pos(&self, inode_num: u32) -> usize {
        // 超级块
        let super_block = self.superblock;
        // 每个块组中包含的inode数量
        let inodes_per_group = super_block.inodes_per_group;
        log::trace!(
            "[ext4:trace] inode_disk_pos: inode={}, inodes_per_group={}",
            inode_num,
            inodes_per_group
        );
        // inode大小
        let inode_size = super_block.inode_size as u64;
        // 组号
        let group = (inode_num - 1) / inodes_per_group;
        // 块组内索引
        let index = (inode_num - 1) % inodes_per_group;
        // 加载块组描述符
        let block_group = self.load_block_group_cached(&super_block, group as usize);
        // 获取inode表块号
        let inode_table_blk_num = block_group.get_inode_table_blk_num();
        // 计算字节偏移量
        inode_table_blk_num as usize * self.block_size + index as usize * inode_size as usize
    }

    /// 直接从磁盘读取 inode，不走 cache。仅用于 get_inode_cached miss 路径。
    pub(crate) fn read_inode_from_disk_uncached(&self, inode_num: u32) -> Ext4InodeRef {
        super::counters::inc_counter!(super::counters::INODE_TABLE_READ);
        debug_assert!(self.block_size == crate::hal::BLOCK_SZ || self.block_size == 1024 || self.block_size == 2048,
            "block_size corrupted: {}", self.block_size);
        let offset = self.inode_disk_pos(inode_num);
        let mut ext4block = self.load_metadata_block_offset(offset);
        let blk_offset = offset % self.block_size;
        let inode: &mut Ext4Inode = ext4block.read_offset_as_mut(blk_offset);
        log::debug!(
            "[Inode Read] Ino: {}, DiskPos: {}, ReadMode: 0o{:o}",
            inode_num,
            offset,
            inode.mode
        );
        Ext4InodeRef {
            inode_num,
            inode: *inode,
        }
    }

    /// 从磁盘加载 inode ref 对象
    ///
    /// Phase 4: 现在是 legacy snapshot wrapper。
    /// 内部委托给 get_inode_snapshot（使用 inode_cache）。
    pub fn get_inode_ref(&self, inode_num: u32) -> Ext4InodeRef {
        self.get_inode_snapshot(inode_num)
    }

    /// 从磁盘加载带 Arc 包装的 inoderef 对象
    ///
    /// Phase 4: 委托给 get_inode_snapshot (走 cache)。
    pub fn get_inode_ref_arc(&self, inode_num: u32) -> Arc<Ext4InodeRef> {
        Arc::new(self.get_inode_snapshot(inode_num))
    }

    /// 带校验和回写 inode 信息
    ///
    /// Phase 4: 先更新 inode_cache，再 mark dirty 并 flush。
    /// 不再直接写盘绕过 cache。
    pub fn write_back_inode(&self, inode_ref: &mut Ext4InodeRef) {
        let ino = inode_ref.inode_num;
        {
            let mut cache = self.inode_cache.lock();
            if let Some(cached) = cache.get(&ino) {
                let mut guard = cached.lock();
                guard.inode = inode_ref.inode;
                guard.dirty = true;
                super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
            } else {
                let offset = self.inode_disk_pos(ino);
                let table_block = offset / self.block_size * self.block_size;
                let blk_offset = offset % self.block_size;
                let cached = CachedExt4Inode {
                    ino,
                    inode: inode_ref.inode,
                    inode_table_block: table_block,
                    offset_in_block: blk_offset,
                    dirty: true,
                };
                cache.insert(ino, alloc::sync::Arc::new(spin::Mutex::new(cached)));
                super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
            }
        }
        let _ = self.flush_inode(ino);
    }

    /// 不带校验和回写 inode 信息
    ///
    /// Phase 4: 先更新 inode_cache 再 flush（跳过 checksum）。
    pub fn write_back_inode_without_csum(&self, inode_ref: &Ext4InodeRef) {
        let ino = inode_ref.inode_num;
        {
            let mut cache = self.inode_cache.lock();
            if let Some(cached) = cache.get(&ino) {
                let mut guard = cached.lock();
                guard.inode = inode_ref.inode;
                guard.dirty = true;
                super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
            } else {
                let offset = self.inode_disk_pos(ino);
                let table_block = offset / self.block_size * self.block_size;
                let blk_offset = offset % self.block_size;
                let cached = CachedExt4Inode {
                    ino,
                    inode: inode_ref.inode,
                    inode_table_block: table_block,
                    offset_in_block: blk_offset,
                    dirty: true,
                };
                cache.insert(ino, alloc::sync::Arc::new(spin::Mutex::new(cached)));
                super::counters::inc_counter!(super::counters::INODE_DIRTY_COUNT);
            }
        }
        let cached = match self.inode_cache.lock().get(&ino).cloned() {
            Some(c) => c,
            None => return,
        };
        let guard = cached.lock();
        if !guard.dirty {
            return;
        }
        let inode_pos = guard.inode_table_block + guard.offset_in_block;
        let on_disk_size = self.superblock.inode_size as usize;
        self.sync_inode_to_metadata_cache(&guard.inode, inode_pos, on_disk_size, guard.ino);
        drop(guard);
        if let Some(cached) = self.inode_cache.lock().get(&ino) {
            cached.lock().dirty = false;
        }
    }

    /// 获取逻辑块号对应的物理块号
    /// # 参数
    /// + inode_ref: &Ext4InodeRef - inode封装对象
    /// + lblock: Ext4Lblk - 逻辑块号
    /// # 返回值
    /// + `Result<Ext4Fsblk>` - 物理块号
    pub fn get_pblock_idx(
        &self,
        inode_ref: &Ext4InodeRef,
        lblock: Ext4Lblk,
    ) -> Result<Ext4Fsblk, isize> {
        // 检查标志位
        let flags = inode_ref.inode.flags();
        if (flags & crate::fs::ext4::EXT4_INODE_FLAG_EXTENTS as u32) == 0 {
            // 如果没有 Extents 标志，说明它是 Fast Symlink 或者旧的 Indirect Block 格式
            let is_link = inode_ref.inode.get_file_type() == DiskInodeType::Link;
            if is_link && inode_ref.inode.size() <= 60 {
                // Fast Symlink 没有数据块，逻辑块 0 也不存在
                return Err(Errno::ENOENT as isize);
            }
            // 如果是普通文件但没有 extents 标志，说明是 Indirect Block 格式，暂不支持
            log::warn!("[ext4] Inode {} does not use extents!", inode_ref.inode_num);
            return Err(Errno::ENOTSUP as isize);
        }
        let search_path = self.find_extent(inode_ref, lblock);
        if let Ok(path) = search_path {
            if let Some(node) = path.path.last() {
                if let Some(extent) = node.extent {
                    let ext_first = extent.get_first_block();
                    let ext_len = extent.get_actual_len() as u32;
                    if lblock >= ext_first && lblock < ext_first + ext_len {
                        let offset_in_ext = lblock - ext_first;
                        return Ok(extent.get_pblock() + offset_in_ext as u64);
                    } else {
                        log::debug!(
                            "[get_pblock_idx] HOLE: inode={}, lblock={}, ext_first={}, ext_len={}",
                            inode_ref.inode_num,
                            lblock,
                            ext_first,
                            ext_len,
                        );
                    }
                } else {
                    log::info!(
                        "[get_pblock_idx] no extent: inode={}, lblock={}",
                        inode_ref.inode_num,
                        lblock,
                    );
                }
            }
        }
        Err(Errno::ENOENT as isize)
    }

    /// 分配一个新的块
    pub fn allocate_new_block(&self, inode_ref: &mut Ext4InodeRef) -> Result<Ext4Fsblk, isize> {
        let mut super_block = self.superblock;
        let inodes_per_group = super_block.inodes_per_group();
        let bgid = (inode_ref.inode_num - 1) / inodes_per_group;
        let index = (inode_ref.inode_num - 1) % inodes_per_group;

        // load block group
        let mut block_group = self.load_block_group_cached(&super_block, bgid as usize);

        let block_bitmap_block = block_group.get_block_bitmap_block(&super_block);

        let mut block_bmap_raw_data = self.read_metadata_block(block_bitmap_block as usize);
        super::counters::inc_counter!(super::counters::BLOCK_BITMAP_READ);
        let data: &mut Vec<u8> = &mut block_bmap_raw_data.to_vec();
        let mut rel_blk_idx = 0;

        ext4_bmap_bit_find_clr(data, index, 0x8000, &mut rel_blk_idx);
        ext4_bmap_bit_set(data, rel_blk_idx);

        block_group.set_block_group_balloc_bitmap_csum(&super_block, data);
        // todo!();
        self.store_metadata_block_dirty(block_bitmap_block as usize, data);
        super::counters::inc_counter!(super::counters::BLOCK_BITMAP_WRITE);

        /* Update superblock free blocks count */
        let mut super_blk_free_blocks = super_block.free_blocks_count();
        super_blk_free_blocks -= 1;
        super_block.set_free_blocks_count(super_blk_free_blocks);
        self.defer_superblock_write(&super_block);

        /* Update inode blocks (different block size!) count */
        let mut inode_blocks = inode_ref.inode.blocks_count();
        // inode_blocks += (self.block_size / EXT4_INODE_BLOCK_SIZE) as u64;
        inode_blocks += (self.block_size / 512) as u64;
        inode_ref.inode.set_blocks_count(inode_blocks);
        self.write_back_inode(inode_ref);

        /* Update block group free blocks count */
        let mut fb_cnt = block_group.get_free_blocks_count();
        fb_cnt -= 1;
        block_group.set_free_blocks_count(fb_cnt as u32);
        self.defer_bg_write(&block_group, bgid as u32, &super_block);

        Ok(rel_blk_idx as Ext4Fsblk)
    }

    /// Append a new block to the inode and update the extent tree.
    ///
    /// Params:
    /// inode_ref: &mut Ext4InodeRef - inode reference
    /// iblock: Ext4Lblk - logical block id
    ///
    /// Returns:
    /// `Result<Ext4Fsblk>` - physical block id of the new block
    pub fn append_inode_pblk(&self, inode_ref: &mut Ext4InodeRef) -> Result<Ext4Fsblk, isize> {
        let inode_size = inode_ref.inode.size();
        let iblock = ((inode_size as usize + self.block_size - 1) / self.block_size) as u32;

        let mut newex: Ext4Extent = Ext4Extent::default();

        let new_block = self.balloc_alloc_block(inode_ref, None)?;

        newex.first_block = iblock;
        newex.store_pblock(new_block);
        newex.block_count = min(1, EXT_MAX_BLOCKS - iblock) as u16;

        self.insert_extent(inode_ref, &mut newex)?;

        // Update the inode size
        let mut inode_size = inode_ref.inode.size();
        inode_size += self.block_size as u64;
        inode_ref.inode.set_size(inode_size);
        self.write_back_inode(inode_ref);

        Ok(new_block)
    }

    /// Append a new block to the inode and update the extent tree.From a specific bgid
    ///
    /// Params:
    /// inode_ref: &mut Ext4InodeRef - inode reference
    /// bgid: Start bgid of free block search
    ///
    /// Returns:
    /// `Result<Ext4Fsblk>` - physical block id of the new block
    pub fn append_inode_pblk_from(
        &self,
        inode_ref: &mut Ext4InodeRef,
        start_bgid: &mut u32,
    ) -> Result<Ext4Fsblk, isize> {
        let inode_size = inode_ref.inode.size();
        let iblock = ((inode_size as usize + self.block_size - 1) / self.block_size) as u32;

        let mut newex: Ext4Extent = Ext4Extent::default();

        let new_block = self.balloc_alloc_block_from(inode_ref, start_bgid)?;

        newex.first_block = iblock;
        newex.store_pblock(new_block);
        newex.block_count = min(1, EXT_MAX_BLOCKS - iblock) as u16;

        self.insert_extent(inode_ref, &mut newex)?;

        // Update the inode size
        let mut inode_size = inode_ref.inode.size();
        inode_size += self.block_size as u64;
        inode_ref.inode.set_size(inode_size);
        self.write_back_inode(inode_ref);

        Ok(new_block)
    }

    /// Insert a new block at a specific logical block index (not necessarily at EOF).
    /// Used for writing to holes (sparse file blocks) that are before the current file size.
    pub fn insert_inode_pblk(
        &self,
        inode_ref: &mut Ext4InodeRef,
        iblock: u32,
    ) -> Result<Ext4Fsblk, isize> {
        let mut newex: Ext4Extent = Ext4Extent::default();
        let new_block = self.balloc_alloc_block(inode_ref, None)?;
        newex.first_block = iblock;
        newex.store_pblock(new_block);
        newex.block_count = 1;
        self.insert_extent(inode_ref, &mut newex)?;

        let inode_size = inode_ref.inode.size();
        let required_size = (iblock as u64 + 1) * self.block_size as u64;
        if required_size > inode_size {
            inode_ref.inode.set_size(required_size);
        }
        self.write_back_inode(inode_ref);
        log::info!(
            "[insert_inode_pblk] inode={}, iblock={}, pblock={}, new_size={}",
            inode_ref.inode_num,
            iblock,
            new_block,
            required_size,
        );
        Ok(new_block)
    }

    /// Deferred variant of insert_inode_pblk: skips the final write_back_inode.
    /// Caller MUST flush the inode later (e.g. write_at's final flush after all
    /// blocks are allocated). Used only by ensure_blocks_allocated → write_at.
    pub fn insert_inode_pblk_deferred(
        &self,
        inode_ref: &mut Ext4InodeRef,
        iblock: u32,
    ) -> Result<Ext4Fsblk, isize> {
        let mut newex: Ext4Extent = Ext4Extent::default();
        let new_block = self.balloc_alloc_block(inode_ref, None)?;
        newex.first_block = iblock;
        newex.store_pblock(new_block);
        newex.block_count = 1;
        self.insert_extent(inode_ref, &mut newex)?;

        let inode_size = inode_ref.inode.size();
        let required_size = (iblock as u64 + 1) * self.block_size as u64;
        if required_size > inode_size {
            inode_ref.inode.set_size(required_size);
        }
        // defer: no write_back_inode — caller flushes once at end
        Ok(new_block)
    }

    /// Insert a new block at a specific logical block index, from a starting bgid.
    pub fn insert_inode_pblk_from(
        &self,
        inode_ref: &mut Ext4InodeRef,
        iblock: u32,
        start_bgid: &mut u32,
    ) -> Result<Ext4Fsblk, isize> {
        let mut newex: Ext4Extent = Ext4Extent::default();
        let new_block = self.balloc_alloc_block_from(inode_ref, start_bgid)?;
        newex.first_block = iblock;
        newex.store_pblock(new_block);
        newex.block_count = 1;
        self.insert_extent(inode_ref, &mut newex)?;

        let inode_size = inode_ref.inode.size();
        let required_size = (iblock as u64 + 1) * self.block_size as u64;
        if required_size > inode_size {
            inode_ref.inode.set_size(required_size);
        }
        self.write_back_inode(inode_ref);
        log::info!(
            "[insert_inode_pblk_from] inode={}, iblock={}, pblock={}, new_size={}",
            inode_ref.inode_num,
            iblock,
            new_block,
            required_size,
        );
        Ok(new_block)
    }

    /// 分配一个新inode
    /// # 参数
    /// + inode_mode: u16 - inode 文件类型和权限
    ///
    /// # 返回值
    /// + `Result<u32>` - inode 号
    pub fn alloc_inode(&self, is_dir: bool) -> Result<u32, isize> {
        self.ialloc_alloc_inode(is_dir)
    }

    pub fn correspond_inode_mode(&self, filetype: u8) -> u16 {
        let file_type = DirEntryType::from_bits(filetype).unwrap();
        match file_type {
            DirEntryType::EXT4_DE_REG_FILE => InodeFileType::S_IFREG.bits(),
            DirEntryType::EXT4_DE_DIR => InodeFileType::S_IFDIR.bits(),
            DirEntryType::EXT4_DE_SYMLINK => InodeFileType::S_IFLNK.bits(),
            DirEntryType::EXT4_DE_CHRDEV => InodeFileType::S_IFCHR.bits(),
            DirEntryType::EXT4_DE_BLKDEV => InodeFileType::S_IFBLK.bits(),
            DirEntryType::EXT4_DE_FIFO => InodeFileType::S_IFIFO.bits(),
            DirEntryType::EXT4_DE_SOCK => InodeFileType::S_IFSOCK.bits(),
            _ => {
                // FIXME: unsupported filetype
                InodeFileType::S_IFREG.bits()
            }
        }
    }
}
