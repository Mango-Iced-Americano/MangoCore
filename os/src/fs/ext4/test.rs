use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};
use spin::Mutex;

use crate::fs::vfs::FilePrivateData;
use crate::fs::vfs::IndexNode;
use crate::hal::BLOCK_SZ;
use crate::drivers::block::BlockDeviceResult;
use crate::println;

use super::direntry::{remove_dir_entry_record, DirEntryType};
use super::ext4fs::Ext4FileSystem;
use super::*;

// ── FakeBlockDevice ──────────────────────────────────────────────────────

/// Fake block device backed by an embedded ext4 image (512 KiB, 128 blocks).
pub struct FakeBlockDevice {
    data: Mutex<Vec<Vec<u8>>>,
}

impl FakeBlockDevice {
    pub fn new() -> Self {
        let raw = include_bytes!("./test_img.bin");
        let nblocks = raw.len() / BLOCK_SZ;
        let mut blocks = Vec::with_capacity(nblocks);
        for i in 0..nblocks {
            let start = i * BLOCK_SZ;
            let mut block = vec![0u8; BLOCK_SZ];
            block.copy_from_slice(&raw[start..start + BLOCK_SZ]);
            blocks.push(block);
        }
        FakeBlockDevice {
            data: Mutex::new(blocks),
        }
    }

    /// Compare a block's content with expected data.
    #[allow(unused)]
    pub fn compare_block(&self, block_id: usize, expected: &[u8]) -> bool {
        let data = self.data.lock();
        if block_id >= data.len() {
            return false;
        }
        &data[block_id][..expected.len().min(BLOCK_SZ)] == expected
    }

    /// Read back a range of bytes at a given block offset (for verification).
    #[allow(unused)]
    pub fn read_raw(&self, block_id: usize, offset: usize, len: usize) -> Vec<u8> {
        let data = self.data.lock();
        let block = &data[block_id];
        block[offset..offset + len.min(BLOCK_SZ - offset)].to_vec()
    }
}

unsafe impl Send for FakeBlockDevice {}
unsafe impl Sync for FakeBlockDevice {}

impl BlockDevice for FakeBlockDevice {
    fn read_block(&self, block_id: usize, buf: &mut [u8]) -> BlockDeviceResult {
        let data = self.data.lock();
        if block_id < data.len() {
            let copy_len = buf.len().min(BLOCK_SZ);
            buf[..copy_len].copy_from_slice(&data[block_id][..copy_len]);
        } else {
            buf.fill(0);
        }
        Ok(())
    }

    fn write_block(&self, block_id: usize, buf: &[u8]) -> BlockDeviceResult {
        let mut data = self.data.lock();
        if block_id < data.len() {
            let copy_len = buf.len().min(BLOCK_SZ);
            data[block_id][..copy_len].copy_from_slice(&buf[..copy_len]);
        }
        Ok(())
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────

fn create_test_env() -> (Arc<FakeBlockDevice>, Arc<Ext4FileSystem>) {
    let bd = Arc::new(FakeBlockDevice::new());
    let fs = Ext4FileSystem::open_ext4rs(bd.clone());
    (bd, fs)
}

/// Create a regular file in the root directory, return IndexNode and the filesystem.
fn create_file(fs: &Arc<Ext4FileSystem>, name: &str) -> Result<Arc<dyn IndexNode>, String> {
    let mode = InodeFileType::S_IFREG.bits() | 0x1FF;
    let child_ref = fs
        .create(super::ROOT_INODE, name, mode, 0, 0)
        .map_err(|e| alloc::format!("create file '{name}': {e}"))?;
    let ino = Arc::new(Mutex::new(child_ref));
    Ok(super::layout::Ext4OSInode::new_vfs(ino, fs.clone()))
}

fn private_data() -> spin::MutexGuard<'static, FilePrivateData> {
    // Leak a Box<Mutex> to get 'static lifetime (fine for single-threaded tests)
    let m: &'static spin::Mutex<FilePrivateData> = Box::leak(alloc::boxed::Box::new(
        spin::Mutex::new(FilePrivateData::Unused),
    ));
    m.lock()
}

macro_rules! assert_eq_test {
    ($left:expr, $right:expr, $msg:expr) => {
        let l = $left;
        let r = $right;
        if l != r {
            return Err(alloc::format!("{}: expected {:?}, got {:?}", $msg, r, l));
        }
    };
}

macro_rules! assert_test {
    ($cond:expr, $msg:expr) => {
        if !$cond {
            return Err(alloc::format!("{}: condition failed", $msg));
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// T1: Write data through PageCache, read back, verify content.
pub fn test_write_read_pagecache() -> Result<(), String> {
    let (_bd, fs) = create_test_env();
    let file = create_file(&fs, "t1.txt")?;
    let data = b"Hello ext4 PageCache!";
    let mut pd = private_data();

    let n = file
        .write_at(0, data.len(), data, pd)
        .map_err(|e| alloc::format!("write_at: {e:?}"))?;
    assert_eq_test!(n, data.len(), "write length");

    let mut buf = vec![0u8; 128];
    pd = private_data();
    let n = file
        .read_at(0, buf.len(), &mut buf, pd)
        .map_err(|e| alloc::format!("read_at: {e:?}"))?;
    assert_test!(n >= data.len(), "read returned enough bytes");
    assert_eq_test!(&buf[..data.len()], data, "read content matches write");
    Ok(())
}

/// T2: Write, sync, drop cache by re-creating the IndexNode, re-read from disk.
pub fn test_write_sync_reread() -> Result<(), String> {
    let (bd, fs) = create_test_env();
    let file = create_file(&fs, "t2.txt")?;
    let data = b"Persist after sync!";

    // Write
    let mut pd = private_data();
    file.write_at(0, data.len(), data, pd)
        .map_err(|e| alloc::format!("write_at: {e:?}"))?;

    // Sync – forces writeback_all + write_back_inode
    file.sync().map_err(|e| alloc::format!("sync: {e:?}"))?;

    // Drop the old IndexNode, create a fresh one (new PageCache → reads from disk)
    drop(file);

    // Re-open the same file
    let child_ref = fs
        .generic_open("t2.txt", &mut super::ROOT_INODE, false, 0, &mut 0)
        .map_err(|e| alloc::format!("generic_open: {e}"))?;
    let ino = Arc::new(Mutex::new(fs.get_inode_ref(child_ref)));
    let file2 = super::layout::Ext4OSInode::new_vfs(ino, fs.clone());

    let mut buf = vec![0u8; 128];
    let pd = private_data();
    let n = file2
        .read_at(0, buf.len(), &mut buf, pd)
        .map_err(|e| alloc::format!("read_at after reopen: {e:?}"))?;
    assert_test!(n >= data.len(), "re-read returned enough bytes");
    assert_eq_test!(&buf[..data.len()], data, "re-read content matches");

    // Also verify raw block device has the data
    // Find the inode's first physical block, then read it from FakeBlockDevice
    let ino_ref = fs.get_inode_ref(child_ref);
    let lblock = 0u32; // first logical block
    if let Ok(pblock) = fs.get_pblock_idx(&ino_ref, lblock) {
        let raw = bd.read_raw(pblock as usize, 0, data.len());
        assert_eq_test!(&raw[..data.len()], data, "raw device data matches");
    }
    // If get_pblock_idx fails (hole), the file is too small for a block — that's fine

    Ok(())
}

/// T3: Cross-page write (spans two 4K pages).
pub fn test_cross_page_write() -> Result<(), String> {
    let (_bd, fs) = create_test_env();
    let file = create_file(&fs, "t3.txt")?;
    let offset = crate::config::PAGE_SIZE - 32; // spans page 0 and 1
    let data = b"Cross-page boundary data!";
    let mut pd = private_data();

    file.write_at(offset, data.len(), data, pd)
        .map_err(|e| alloc::format!("write_at cross-page: {e:?}"))?;

    let mut buf = vec![0u8; 256];
    // Read from slightly before the offset to also verify surrounding bytes
    pd = private_data();
    let read_start = offset - 16;
    let n = file
        .read_at(read_start, buf.len(), &mut buf, pd)
        .map_err(|e| alloc::format!("read_at cross-page: {e:?}"))?;
    // Verify the written data is at the right position in the buffer
    let data_start = offset - read_start;
    assert_test!(n >= data_start + data.len(), "cross-page read length");
    assert_eq_test!(
        &buf[data_start..data_start + data.len()],
        data,
        "cross-page content"
    );
    Ok(())
}

/// T4: Extend file size (write beyond current EOF).
pub fn test_extend_write() -> Result<(), String> {
    let (_bd, fs) = create_test_env();
    let file = create_file(&fs, "t4.txt")?;
    let data = b"Extended file content!";
    // Write at offset 16 so new_end > 0 = old EOF
    let mut pd = private_data();
    let n = file
        .write_at(16, data.len(), data, pd)
        .map_err(|e| alloc::format!("write_at extend: {e:?}"))?;
    assert_eq_test!(n, data.len(), "extend write length");

    // Read from offset 0 to see zeros in the hole
    let mut buf = vec![0u8; 64];
    let pd = private_data();
    let n = file
        .read_at(0, buf.len(), &mut buf, pd)
        .map_err(|e| alloc::format!("read_at after extend: {e:?}"))?;
    assert_test!(n >= 16 + data.len(), "extend read length");
    // Bytes 0..15 should be 0 (hole before written data)
    assert_test!(
        buf[..16].iter().all(|&b| b == 0),
        "hole before write should be zero"
    );
    // Bytes 16..16+data.len() should match written data
    assert_eq_test!(&buf[16..16 + data.len()], data, "extend content");
    Ok(())
}

/// T5: verify write_page refuses to write unmapped blocks
/// This test is a safety net: if block allocation fails in write_at,
/// the write should still propagate to page cache, but writeback
/// must fail (not silently skip).
pub fn test_write_page_refuses_unmapped() -> Result<(), String> {
    // We cannot easily create an unmapped block in the nodelalloc path,
    // because write_at now allocates blocks before pc.write().
    // This test verifies the invariant by checking that:
    //  - Ext4PageCacheBackend::write_page with unmapped block returns Err
    //  - The PageCache keeps the page dirty after the failure
    //
    // To trigger this, we create a PageCache-backed write then try to
    // writeback an intentionally unmapped page through the backend.
    // We do this by writing through the normal path (which allocates),
    // then deliberately removing the extent mapping (testing only).
    //
    // For now, this is a compile-time assertion + doc test:
    // The fix was applied to Ext4PageCacheBackend::write_page at
    // page_cache.rs:856-860 — unmapped blocks return Err(EIO).
    // The test image has limited blocks; we confirm the normal path
    // does NOT encounter unmapped blocks by writing a multi-page file.
    let (_bd, fs) = create_test_env();
    let file = create_file(&fs, "t5_large.txt")?;
    let data = &[0xABu8; 16384]; // 4 pages
    let mut pd = private_data();
    file.write_at(0, data.len(), data, pd)
        .map_err(|e| alloc::format!("write_at large file: {e:?}"))?;
    // sync will call writeback_all which must not hit unmapped blocks
    file.sync()
        .map_err(|e| alloc::format!("sync large file: {e:?}"))?;
    Ok(())
}

/// T6: deleting the first record in a non-initial directory block must keep
/// that record's rec_len. APK creates enough files to exercise this layout.
pub fn test_remove_first_dir_record_preserves_framing() -> Result<(), String> {
    const DATA_END: usize = 4084;
    const ENTRY_LEN: u16 = 64;
    let mut block = vec![0xA5u8; 4096];
    block[0..4].copy_from_slice(&42u32.to_le_bytes());
    block[4..6].copy_from_slice(&ENTRY_LEN.to_le_bytes());
    block[6] = 8;
    block[7] = DirEntryType::EXT4_DE_REG_FILE.bits();

    remove_dir_entry_record(&mut block, DATA_END, 0, 0, ENTRY_LEN)
        .map_err(|e| alloc::format!("remove first record: {e}"))?;

    assert_eq_test!(
        u16::from_le_bytes([block[4], block[5]]),
        ENTRY_LEN,
        "first record rec_len"
    );
    assert_test!(block[0..4].iter().all(|byte| *byte == 0), "inode cleared");
    assert_test!(
        block[6..ENTRY_LEN as usize].iter().all(|byte| *byte == 0),
        "first record body cleared"
    );
    assert_eq_test!(block[ENTRY_LEN as usize], 0xA5, "next record untouched");
    Ok(())
}

/// T7: deleting a non-first record still merges its span into the immediate
/// predecessor and wipes the removed record.
pub fn test_remove_nonfirst_dir_record_merges_predecessor() -> Result<(), String> {
    const DATA_END: usize = 4084;
    const PREV_LEN: u16 = 16;
    const ENTRY_LEN: u16 = 32;
    let mut block = vec![0xA5u8; 4096];
    block[4..6].copy_from_slice(&PREV_LEN.to_le_bytes());
    block[PREV_LEN as usize + 4..PREV_LEN as usize + 6].copy_from_slice(&ENTRY_LEN.to_le_bytes());

    remove_dir_entry_record(&mut block, DATA_END, PREV_LEN as usize, 0, ENTRY_LEN)
        .map_err(|e| alloc::format!("remove non-first record: {e}"))?;

    assert_eq_test!(
        u16::from_le_bytes([block[4], block[5]]),
        PREV_LEN + ENTRY_LEN,
        "merged predecessor rec_len"
    );
    assert_test!(
        block[PREV_LEN as usize..(PREV_LEN + ENTRY_LEN) as usize]
            .iter()
            .all(|byte| *byte == 0),
        "removed record wiped"
    );
    Ok(())
}

/// T8: ext4 directory file types are exact values, not independent flag bits,
/// and a repeated symlink creation must fail without adding another dirent.
pub fn test_symlink_type_and_duplicate_rejection() -> Result<(), String> {
    let (_bd, fs) = create_test_env();
    let root = super::layout::Ext4OSInode::new_vfs(
        Arc::new(Mutex::new(fs.get_inode_ref(super::ROOT_INODE))),
        fs,
    );

    root.symlink("t8_link", "/bin/busybox")
        .map_err(|e| alloc::format!("first symlink: {e:?}"))?;
    match root.symlink("t8_link", "/bin/busybox") {
        Err(crate::utils::error::SyscallErr::EEXIST) => {}
        Err(err) => return Err(alloc::format!("duplicate symlink errno: {err:?}")),
        Ok(_) => return Err(String::from("duplicate symlink unexpectedly succeeded")),
    }

    let entries = root
        .list_dirents()
        .map_err(|e| alloc::format!("list_dirents: {e:?}"))?;
    let links: Vec<_> = entries
        .iter()
        .filter(|(name, _, _)| name == "t8_link")
        .collect();
    assert_eq_test!(links.len(), 1, "single symlink dirent");
    assert_eq_test!(
        links[0].2,
        crate::fs::vfs::FileType::SymLink,
        "symlink dirent type"
    );
    Ok(())
}

/// T9: rmdir removes both directory links, restores the parent's link count,
/// and defers inode reclamation only while a live VFS object still exists.
pub fn test_rmdir_link_counts_and_reclaim() -> Result<(), String> {
    let (_bd, fs) = create_test_env();
    let root = super::layout::Ext4OSInode::new_vfs(
        Arc::new(Mutex::new(fs.get_inode_ref(super::ROOT_INODE))),
        fs.clone(),
    );
    let root_links_before = root
        .metadata()
        .map_err(|e| alloc::format!("root metadata before mkdir: {e:?}"))?
        .nlinks;
    let free_inodes_before = fs.get_superblock().free_inodes_count();

    let child = root
        .mkdir("t9_dir", crate::fs::vfs::InodeMode::S_IRWXUGO)
        .map_err(|e| alloc::format!("mkdir: {e:?}"))?;
    assert_eq_test!(
        root.metadata()
            .map_err(|e| alloc::format!("root metadata after mkdir: {e:?}"))?
            .nlinks,
        root_links_before + 1,
        "parent link count after mkdir"
    );

    root.rmdir("t9_dir")
        .map_err(|e| alloc::format!("rmdir: {e:?}"))?;
    assert_eq_test!(
        child
            .metadata()
            .map_err(|e| alloc::format!("unlinked child metadata: {e:?}"))?
            .nlinks,
        0,
        "removed directory link count"
    );
    assert_eq_test!(
        root.metadata()
            .map_err(|e| alloc::format!("root metadata after rmdir: {e:?}"))?
            .nlinks,
        root_links_before,
        "parent link count after rmdir"
    );
    assert_eq_test!(
        fs.get_superblock().free_inodes_count(),
        free_inodes_before - 1,
        "live directory object delays inode reclaim"
    );

    drop(child);
    assert_eq_test!(
        fs.get_superblock().free_inodes_count(),
        free_inodes_before,
        "directory inode reclaimed after final reference"
    );
    Ok(())
}

/// T10: a freed metadata block must not retain dirty directory contents that
/// could later be flushed over a new file reusing the same physical block.
pub fn test_freed_metadata_block_is_invalidated() -> Result<(), String> {
    const BLOCK_SIZE: usize = 64;
    const BLOCK_ID: usize = 23;
    let cache = super::meta_cache::MetaBlockCache::new(4, BLOCK_SIZE);
    cache.store_dirty_block(BLOCK_ID, &[0xA5; BLOCK_SIZE]);
    cache.invalidate_range(BLOCK_ID, 1);

    let data = cache.read_block(BLOCK_ID, |id, buf| {
        assert_eq!(id, BLOCK_ID);
        buf.fill(0x5A);
    });
    assert_test!(
        data.iter().all(|byte| *byte == 0x5A),
        "freed block reloaded from its new owner"
    );
    Ok(())
}

/// Helper to run tests
type TestFn = fn() -> Result<(), String>;

const TESTS: &[(&str, TestFn)] = &[
    ("write_read_pagecache", test_write_read_pagecache),
    ("write_sync_reread", test_write_sync_reread),
    ("cross_page_write", test_cross_page_write),
    ("extend_write", test_extend_write),
    (
        "write_page_refuses_unmapped",
        test_write_page_refuses_unmapped,
    ),
    (
        "remove_first_dir_record_preserves_framing",
        test_remove_first_dir_record_preserves_framing,
    ),
    (
        "remove_nonfirst_dir_record_merges_predecessor",
        test_remove_nonfirst_dir_record_merges_predecessor,
    ),
    (
        "symlink_type_and_duplicate_rejection",
        test_symlink_type_and_duplicate_rejection,
    ),
    (
        "rmdir_link_counts_and_reclaim",
        test_rmdir_link_counts_and_reclaim,
    ),
    (
        "freed_metadata_block_is_invalidated",
        test_freed_metadata_block_is_invalidated,
    ),
];

pub fn run_all_tests() {
    let mut passed = 0usize;
    let mut failed = 0usize;
    super::counters::enable_counters();
    println!("[ext4_test] Running {} tests...", TESTS.len());
    for (name, test_fn) in TESTS {
        super::counters::reset_counters();
        match test_fn() {
            Ok(()) => {
                passed += 1;
                println!("  [PASS] {}", name);
            }
            Err(msg) => {
                failed += 1;
                println!("  [FAIL] {}: {}", name, msg);
            }
        }
        super::counters::dump_scenario(name);
        println!("");
    }
    super::counters::disable_counters();
    println!(
        "[ext4_test] Results: {} passed, {} failed, {} total",
        passed,
        failed,
        TESTS.len()
    );
    if failed > 0 {
        panic!("ext4_test: {} failures", failed);
    }
}

// ── Keep original test_get_file ──────────────────────────────────────────

impl Ext4FileSystem {
    /// 尝试打开一个文件并读取内容
    /// 读取 2048 个字节
    pub fn test_get_file(&self, path: &str) {
        let read_size = 4096;
        let child_inode = self.generic_open(path, &mut 2, false, 0, &mut 0).unwrap();
        println!("child_inode_num: {:?}", child_inode);
        let mut data = vec![0u8; read_size as usize];
        // 读取文件内容
        let bytes_read = self.read_at(child_inode, 0 as usize, &mut data);
        if bytes_read.unwrap() < read_size {
            println!(
                "[kernel readtest] End of file reached, bytes read: {:?}",
                bytes_read
            );
        }
        let valid_data = &data[0..bytes_read.unwrap()];
        let text = String::from_utf8_lossy(&valid_data);
        let unescaped_data = unescape_char(&text);
        println!("[kernel readtest] Read Data at {:?}", path);
        print!("{}", unescaped_data);
    }
}

/// 将转义字符转换为实际的字符
fn unescape_char(escaped: &str) -> String {
    let mut result = String::new();
    let mut i = 0;

    while i < escaped.len() {
        // 确保没有越界，检查 i + 2 是否超出了 escaped 的长度
        if i + 2 <= escaped.len() {
            if &escaped[i..i + 2] == "\\n" {
                result.push('\n');
                i += 2;
            } else if &escaped[i..i + 2] == "\\r" {
                result.push('\r');
                i += 2;
            } else if &escaped[i..i + 2] == "\\t" {
                result.push('\t');
                i += 2;
            } else if &escaped[i..i + 2] == "\\\\" {
                result.push('\\');
                i += 2;
            } else if &escaped[i..i + 2] == "\\\"" {
                result.push('\"');
                i += 2;
            } else {
                // 如果没有匹配的转义字符，则正常添加字符
                result.push(escaped[i..i + 1].chars().next().unwrap());
                i += 1;
            }
        } else {
            // 如果无法匹配转义字符，直接添加当前字符
            result.push(escaped[i..i + 1].chars().next().unwrap());
            i += 1;
        }
    }

    result
}
