//! ext4 性能归因计数器
//!
//! 用于评估每个缓存层和 I/O 路径的实际效果。
//! 每个测试场景前应调用 reset_counters() 清零。

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ── 总开关 ───────────────────────────────────────────────────────────────

static COUNTERS_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn enable_counters() { COUNTERS_ENABLED.store(true, Ordering::Relaxed); }
pub fn disable_counters() { COUNTERS_ENABLED.store(false, Ordering::Relaxed); }
pub fn counters_enabled() -> bool { COUNTERS_ENABLED.load(Ordering::Relaxed) }

// ── 总 block I/O ───────────────────────────────────────────────────────

pub static BLOCK_READ_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_WRITE_TOTAL: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_READ_COUNT: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);

// ── VFS inode object cache ──────────────────────────────────────────────

pub static INODE_OBJ_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_INSERT: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_REMOVE: AtomicU64 = AtomicU64::new(0);
pub static INODE_OBJ_INVALIDATE: AtomicU64 = AtomicU64::new(0);

// ── Directory children cache ────────────────────────────────────────────

pub static DIR_CHILDREN_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_INSERT: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_REMOVE: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_INVALIDATE: AtomicU64 = AtomicU64::new(0);
pub static DIR_CHILDREN_STALE_WEAK: AtomicU64 = AtomicU64::new(0);

// ── Cache lifecycle counters (Phase 2+) ──────────────────────────────────

pub static INODE_OBJ_STALE: AtomicU64 = AtomicU64::new(0);
pub static PAGE_CACHE_STALE: AtomicU64 = AtomicU64::new(0);

// ── Inode cache capacity (Phase 3) ────────────────────────────────────────

pub static INODE_CACHE_INSERT: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_EVICT_CLEAN: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_EVICT_DIRTY_FLUSH: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_EVICT_FAILED_DIRTY: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_REMOVE_UNLINKED: AtomicU64 = AtomicU64::new(0);

// ── Per-inode metadata cache ────────────────────────────────────────────

pub static INODE_META_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static INODE_META_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_TARGET_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_TARGET_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static METADATA_DIRTY_MARK: AtomicU64 = AtomicU64::new(0);
pub static METADATA_FLUSH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_FLUSH_ERROR: AtomicU64 = AtomicU64::new(0);

// ── Metadata block cache ─────────────────────────────────────────────────

pub static METADATA_BLOCK_READ_COUNT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_BLOCK_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_BLOCK_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_BLOCK_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static METADATA_DIRTY_BLOCK_COUNT: AtomicU64 = AtomicU64::new(0);
pub static METADATA_FLUSH_IMMEDIATE_COUNT: AtomicU64 = AtomicU64::new(0);

// ── Inode table cache (Phase 4) ─────────────────────────────────────────

pub static INODE_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static INODE_CACHE_FLUSH: AtomicU64 = AtomicU64::new(0);
pub static INODE_LOAD_COUNT: AtomicU64 = AtomicU64::new(0);
pub static INODE_DIRTY_COUNT: AtomicU64 = AtomicU64::new(0);
pub static INODE_FLUSH_COUNT: AtomicU64 = AtomicU64::new(0);

// ── Dentry/lookup ────────────────────────────────────────────────────────

pub static DENTRY_LOOKUP_COUNT: AtomicU64 = AtomicU64::new(0);
pub static DENTRY_CACHE_HIT: AtomicU64 = AtomicU64::new(0);
pub static DENTRY_CACHE_MISS: AtomicU64 = AtomicU64::new(0);
pub static NEGATIVE_DENTRY_HIT: AtomicU64 = AtomicU64::new(0);
pub static NEGATIVE_DENTRY_INSERT: AtomicU64 = AtomicU64::new(0);

// ── Directory scan ───────────────────────────────────────────────────────

pub static DIR_LOOKUP_COUNT: AtomicU64 = AtomicU64::new(0);
pub static DIR_FULL_SCAN_COUNT: AtomicU64 = AtomicU64::new(0);
pub static DIR_FULL_SCAN_ENTRIES: AtomicU64 = AtomicU64::new(0);
pub static CACHE_ALL_SUBFILE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static CACHE_ALL_SUBFILE_ENTRIES_LOADED: AtomicU64 = AtomicU64::new(0);
pub static CACHE_ALL_SUBFILE_INODE_LOADS: AtomicU64 = AtomicU64::new(0);

// ── Getdents ─────────────────────────────────────────────────────────────

pub static GETDENTS_CALL_COUNT: AtomicU64 = AtomicU64::new(0);
pub static GETDENTS_RETURNED_ENTRIES: AtomicU64 = AtomicU64::new(0);
pub static GETDENTS_RETURNED_BYTES: AtomicU64 = AtomicU64::new(0);
pub static GETDENTS_INVALID_RECLEN_COUNT: AtomicU64 = AtomicU64::new(0);

// ── 底层 metadata block I/O 分类 ────────────────────────────────────────

pub static INODE_TABLE_READ: AtomicU64 = AtomicU64::new(0);
pub static INODE_TABLE_WRITE: AtomicU64 = AtomicU64::new(0);
pub static INODE_BITMAP_READ: AtomicU64 = AtomicU64::new(0);
pub static INODE_BITMAP_WRITE: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_BITMAP_READ: AtomicU64 = AtomicU64::new(0);
pub static BLOCK_BITMAP_WRITE: AtomicU64 = AtomicU64::new(0);
pub static DIR_BLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static DIR_BLOCK_WRITE: AtomicU64 = AtomicU64::new(0);
pub static READDIR_DIR_BLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static GROUP_DESC_READ: AtomicU64 = AtomicU64::new(0);
pub static GROUP_DESC_WRITE: AtomicU64 = AtomicU64::new(0);
pub static SUPERBLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static SUPERBLOCK_WRITE: AtomicU64 = AtomicU64::new(0);

// ── Data block I/O 分类 ──────────────────────────────────────────────────
// 仅在物理 I/O 层计数（PageCacheBackend / direct block write），不在逻辑层双计

pub static DATA_BLOCK_READ: AtomicU64 = AtomicU64::new(0);
pub static DATA_BLOCK_WRITE: AtomicU64 = AtomicU64::new(0);

// ── 未分类 metadata I/O ──────────────────────────────────────────────────
// 用于 extent tree block、checksum block、xattr 等尚未单独分类的路径

pub static OTHER_META_READ: AtomicU64 = AtomicU64::new(0);
pub static OTHER_META_WRITE: AtomicU64 = AtomicU64::new(0);

// ── Symlink-specific ─────────────────────────────────────────────────────

pub static SYMLINK_CREATE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static FAST_SYMLINK_CREATE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_READLINK_COUNT: AtomicU64 = AtomicU64::new(0);
pub static FAST_SYMLINK_READ_INLINE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_INODE_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_PARENT_INODE_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_DIR_BLOCK_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SYMLINK_SLOW_COUNT: AtomicU64 = AtomicU64::new(0);

// ── 辅助 ─────────────────────────────────────────────────────────────────

macro_rules! inc_counter {
    ($counter:expr) => {
        if $crate::fs::ext4::counters::counters_enabled() {
            $counter.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
    };
}

pub(crate) use inc_counter;

/// 重置所有计数器（每个测试场景前调用）
pub fn reset_counters() {
    let all = [
        &BLOCK_READ_TOTAL, &BLOCK_WRITE_TOTAL,
        &BLOCK_READ_COUNT, &BLOCK_WRITE_COUNT,
        &INODE_OBJ_CACHE_HIT, &INODE_OBJ_CACHE_MISS, &INODE_OBJ_INSERT,
        &INODE_OBJ_REMOVE, &INODE_OBJ_INVALIDATE,
        &DIR_CHILDREN_CACHE_HIT, &DIR_CHILDREN_CACHE_MISS, &DIR_CHILDREN_INSERT,
        &DIR_CHILDREN_REMOVE, &DIR_CHILDREN_INVALIDATE, &DIR_CHILDREN_STALE_WEAK,
        &INODE_OBJ_STALE, &PAGE_CACHE_STALE,
        &INODE_CACHE_INSERT, &INODE_CACHE_EVICT_CLEAN,
        &INODE_CACHE_EVICT_DIRTY_FLUSH, &INODE_CACHE_EVICT_FAILED_DIRTY,
        &INODE_CACHE_REMOVE_UNLINKED,
        &INODE_META_CACHE_HIT, &INODE_META_CACHE_MISS,
        &SYMLINK_TARGET_CACHE_HIT, &SYMLINK_TARGET_CACHE_MISS,
        &METADATA_DIRTY_MARK, &METADATA_FLUSH_COUNT, &METADATA_FLUSH_ERROR,
        &METADATA_BLOCK_READ_COUNT, &METADATA_BLOCK_WRITE_COUNT,
        &METADATA_BLOCK_CACHE_HIT, &METADATA_BLOCK_CACHE_MISS,
        &METADATA_DIRTY_BLOCK_COUNT, &METADATA_FLUSH_IMMEDIATE_COUNT,
        &INODE_CACHE_HIT, &INODE_CACHE_MISS, &INODE_CACHE_FLUSH,
        &INODE_LOAD_COUNT, &INODE_DIRTY_COUNT, &INODE_FLUSH_COUNT,
        &DENTRY_LOOKUP_COUNT, &DENTRY_CACHE_HIT, &DENTRY_CACHE_MISS,
        &NEGATIVE_DENTRY_HIT, &NEGATIVE_DENTRY_INSERT,
        &DIR_LOOKUP_COUNT, &DIR_FULL_SCAN_COUNT, &DIR_FULL_SCAN_ENTRIES,
        &CACHE_ALL_SUBFILE_COUNT, &CACHE_ALL_SUBFILE_ENTRIES_LOADED,
        &CACHE_ALL_SUBFILE_INODE_LOADS,
        &GETDENTS_CALL_COUNT, &GETDENTS_RETURNED_ENTRIES,
        &GETDENTS_RETURNED_BYTES, &GETDENTS_INVALID_RECLEN_COUNT,
        &INODE_TABLE_READ, &INODE_TABLE_WRITE,
        &INODE_BITMAP_READ, &INODE_BITMAP_WRITE,
        &BLOCK_BITMAP_READ, &BLOCK_BITMAP_WRITE,
        &DIR_BLOCK_READ, &DIR_BLOCK_WRITE,
        &READDIR_DIR_BLOCK_READ,
        &GROUP_DESC_READ, &GROUP_DESC_WRITE,
        &SUPERBLOCK_READ, &SUPERBLOCK_WRITE,
        &DATA_BLOCK_READ, &DATA_BLOCK_WRITE,
        &OTHER_META_READ, &OTHER_META_WRITE,
        &SYMLINK_CREATE_COUNT, &FAST_SYMLINK_CREATE_COUNT,
        &SYMLINK_READLINK_COUNT, &FAST_SYMLINK_READ_INLINE_COUNT,
        &SYMLINK_INODE_WRITE_COUNT, &SYMLINK_PARENT_INODE_WRITE_COUNT,
        &SYMLINK_DIR_BLOCK_WRITE_COUNT, &SYMLINK_SLOW_COUNT,
    ];
    for c in &all {
        c.store(0, Ordering::Relaxed);
    }
}

/// 场景化输出 — 单行紧凑格式便于脚本解析
pub fn dump_scenario(label: &str) {
    println!("=== ext4 I/O Profile: {} ===", label);
    println!("block_read_total={} block_write_total={}",
        BLOCK_READ_TOTAL.load(Ordering::Relaxed),
        BLOCK_WRITE_TOTAL.load(Ordering::Relaxed));
    println!("ino_tbl r={} w={} | ino_bmp r={} w={} | blk_bmp r={} w={}",
        INODE_TABLE_READ.load(Ordering::Relaxed), INODE_TABLE_WRITE.load(Ordering::Relaxed),
        INODE_BITMAP_READ.load(Ordering::Relaxed), INODE_BITMAP_WRITE.load(Ordering::Relaxed),
        BLOCK_BITMAP_READ.load(Ordering::Relaxed), BLOCK_BITMAP_WRITE.load(Ordering::Relaxed));
    println!("dir r={} w={} | gd r={} w={} | sb r={} w={}",
        DIR_BLOCK_READ.load(Ordering::Relaxed), DIR_BLOCK_WRITE.load(Ordering::Relaxed),
        GROUP_DESC_READ.load(Ordering::Relaxed), GROUP_DESC_WRITE.load(Ordering::Relaxed),
        SUPERBLOCK_READ.load(Ordering::Relaxed), SUPERBLOCK_WRITE.load(Ordering::Relaxed));
    println!("data r={} w={}",
        DATA_BLOCK_READ.load(Ordering::Relaxed), DATA_BLOCK_WRITE.load(Ordering::Relaxed));
    println!("readdir_dir r={}",
        READDIR_DIR_BLOCK_READ.load(Ordering::Relaxed));
    println!("inode_obj hit={} miss={} | children hit={} miss={} stale_weak={}",
        INODE_OBJ_CACHE_HIT.load(Ordering::Relaxed), INODE_OBJ_CACHE_MISS.load(Ordering::Relaxed),
        DIR_CHILDREN_CACHE_HIT.load(Ordering::Relaxed), DIR_CHILDREN_CACHE_MISS.load(Ordering::Relaxed),
        DIR_CHILDREN_STALE_WEAK.load(Ordering::Relaxed));
    println!("inode_cache hit={} miss={} flush={} | meta hit={} miss={}",
        INODE_CACHE_HIT.load(Ordering::Relaxed), INODE_CACHE_MISS.load(Ordering::Relaxed),
        INODE_CACHE_FLUSH.load(Ordering::Relaxed),
        INODE_META_CACHE_HIT.load(Ordering::Relaxed), INODE_META_CACHE_MISS.load(Ordering::Relaxed));
    println!("symlink_target hit={} miss={} | sym_create={} fast={} readlink={} inline={}",
        SYMLINK_TARGET_CACHE_HIT.load(Ordering::Relaxed), SYMLINK_TARGET_CACHE_MISS.load(Ordering::Relaxed),
        SYMLINK_CREATE_COUNT.load(Ordering::Relaxed), FAST_SYMLINK_CREATE_COUNT.load(Ordering::Relaxed),
        SYMLINK_READLINK_COUNT.load(Ordering::Relaxed), FAST_SYMLINK_READ_INLINE_COUNT.load(Ordering::Relaxed));
    println!("symlink_io ino_w={} parent_w={} dir_w={}",
        SYMLINK_INODE_WRITE_COUNT.load(Ordering::Relaxed),
        SYMLINK_PARENT_INODE_WRITE_COUNT.load(Ordering::Relaxed),
        SYMLINK_DIR_BLOCK_WRITE_COUNT.load(Ordering::Relaxed));
    println!("block_read_count={} block_write_count={}",
        BLOCK_READ_COUNT.load(Ordering::Relaxed),
        BLOCK_WRITE_COUNT.load(Ordering::Relaxed));
    println!("metadata_block_read_count={} metadata_block_write_count={} metadata_block_cache_hit={} metadata_block_cache_miss={} metadata_dirty_block_count={} metadata_flush_immediate_count={}",
        METADATA_BLOCK_READ_COUNT.load(Ordering::Relaxed),
        METADATA_BLOCK_WRITE_COUNT.load(Ordering::Relaxed),
        METADATA_BLOCK_CACHE_HIT.load(Ordering::Relaxed),
        METADATA_BLOCK_CACHE_MISS.load(Ordering::Relaxed),
        METADATA_DIRTY_BLOCK_COUNT.load(Ordering::Relaxed),
        METADATA_FLUSH_IMMEDIATE_COUNT.load(Ordering::Relaxed));
    println!("inode_load_count={} inode_dirty_count={} inode_flush_count={}",
        INODE_LOAD_COUNT.load(Ordering::Relaxed),
        INODE_DIRTY_COUNT.load(Ordering::Relaxed),
        INODE_FLUSH_COUNT.load(Ordering::Relaxed));
    println!("dentry_lookup_count={} dentry_cache_hit={} dentry_cache_miss={} negative_dentry_hit={} negative_dentry_insert={}",
        DENTRY_LOOKUP_COUNT.load(Ordering::Relaxed),
        DENTRY_CACHE_HIT.load(Ordering::Relaxed),
        DENTRY_CACHE_MISS.load(Ordering::Relaxed),
        NEGATIVE_DENTRY_HIT.load(Ordering::Relaxed),
        NEGATIVE_DENTRY_INSERT.load(Ordering::Relaxed));
    println!("dir_lookup_count={} dir_full_scan_count={} dir_full_scan_entries={}",
        DIR_LOOKUP_COUNT.load(Ordering::Relaxed),
        DIR_FULL_SCAN_COUNT.load(Ordering::Relaxed),
        DIR_FULL_SCAN_ENTRIES.load(Ordering::Relaxed));
    println!("cache_all_subfile_count={} cache_all_subfile_entries_loaded={} cache_all_subfile_inode_loads={}",
        CACHE_ALL_SUBFILE_COUNT.load(Ordering::Relaxed),
        CACHE_ALL_SUBFILE_ENTRIES_LOADED.load(Ordering::Relaxed),
        CACHE_ALL_SUBFILE_INODE_LOADS.load(Ordering::Relaxed));
    println!("getdents_call_count={} getdents_returned_entries={} getdents_returned_bytes={} getdents_invalid_reclen_count={}",
        GETDENTS_CALL_COUNT.load(Ordering::Relaxed),
        GETDENTS_RETURNED_ENTRIES.load(Ordering::Relaxed),
        GETDENTS_RETURNED_BYTES.load(Ordering::Relaxed),
        GETDENTS_INVALID_RECLEN_COUNT.load(Ordering::Relaxed));
    println!("symlink_slow_count={}",
        SYMLINK_SLOW_COUNT.load(Ordering::Relaxed));
}

// ── syscall 接口 ──────────────────────────────────────────────────────────

/// sys_ext4_counters(cmd, label_ptr, label_len):
///   cmd=0: enable,  cmd=1: disable,  cmd=2: reset,  cmd=3: dump I/O profile
///   cmd=4: begin_meta_batch, cmd=5: end_meta_batch_and_flush, cmd=7: abort_meta_batch
pub fn sys_ext4_counters(cmd: usize, label_ptr: usize, label_len: usize) -> isize {
    match cmd {
        0 => { enable_counters(); 0 }
        1 => { disable_counters(); 0 }
        2 => { reset_counters(); 0 }
        3 => {
            let label = read_label(label_ptr, label_len);
            dump_scenario(&label);
            0
        }
        4 | 5 | 7 => {
            // DISABLED: meta_batch has known cumulative accounting issue
            // (ialloc_alloc_inode reloads bg from disk, batch only writes -1 not -N)
            // Re-enable after fixing cumulative bg/sb state tracking
            -38 // ENOSYS
        }
        6 => {
            let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
            let fs = match guard.as_ref().and_then(|w| w.upgrade()) {
                Some(fs) => fs,
                None => return -6, // ENXIO
            };
            drop(guard);
            let label = read_label(label_ptr, label_len);
            fs.dump_cache_memory_profile(&label);
            0
        }
        8 => {
            // prune_stale_weak_entries: clean inode_objects dead weak + stale page_caches
            let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
            let fs = match guard.as_ref().and_then(|w| w.upgrade()) {
                Some(fs) => fs,
                None => return -6,
            };
            drop(guard);
            let (io, pc) = fs.prune_stale_weak_entries();
            io as isize + pc as isize
        }
        9 => {
            // clear_all_children_caches
            let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
            let fs = match guard.as_ref().and_then(|w| w.upgrade()) {
                Some(fs) => fs,
                None => return -6,
            };
            drop(guard);
            fs.clear_all_children_caches() as isize
        }
        _ => -22, // EINVAL
    }
}

fn read_label(label_ptr: usize, label_len: usize) -> alloc::string::String {
    if label_ptr != 0 && label_len > 0 && label_len <= 64 {
        let token = crate::task::current_user_token();
        crate::mm::translated_str(
            token,
            label_ptr as *const u8,
        ).unwrap_or_else(|_| alloc::string::String::from("unknown"))
    } else {
        alloc::string::String::from("unknown")
    }
}

/// 通过全局 Ext4FileSystem 引用触发 batch mode（syscall cmd 4/5）
pub fn sys_ext4_meta_batch(cmd: usize) -> isize {
    let guard = crate::fs::ext4::ext4fs::GLOBAL_EXT4FS.lock();
    let fs = match guard.as_ref().and_then(|w| w.upgrade()) {
        Some(fs) => fs,
        None => return -6, // ENXIO — no ext4 fs mounted
    };
    drop(guard);
    match cmd {
        4 => { fs.begin_meta_batch(); 0 }
        5 => { fs.end_meta_batch_and_flush(); 0 }
        _ => -22,
    }
}
