# Metadata Write Amplification Reduction Plan

## Current Baseline (51/51 FS test passed)

| Profile | ino_tbl_w | flush | gd_w | sb_w |
|---------|-----------|-------|------|------|
| stress_create_many (50 files) | 509 | 305 | 153 | 153 |
| stress_large_file (64KB) | 112 | 104 | 21 | 21 |
| single fast symlink | 2 | 2 | 1 | 1 |

## Phase 1: Fix INODE_TABLE_WRITE double-count (P0)

**Root cause**: `write_back_inode_without_csum` calls `sync_inode_to_disk` (which increments `INODE_TABLE_WRITE` at ext4_inode.rs:623), then also increments it again at ext4_inode.rs:789.

**Fix**: Remove the duplicate `inc_counter!(INODE_TABLE_WRITE)` from `write_back_inode_without_csum` (or from `sync_inode_to_disk`, pick one). The counter should only increment once per physical inode table write.

**Expected**: `ino_tbl_w` drops from 509 → ~407 for create_many. No behavioral change.

**QEMU verify**: stress_create_many profile, 51/51 still pass

---

## Phase 2: Coalesce create() child inode flushes (P1)

**Current create() (file.rs:292-301):**
```
create_inode → write_back_inode_without_csum (flush #1: empty child)
link → write_back_inode parent (flush #2)
write_back_inode child (flush #3: final)
```

**Fix**: 
1. Replace `link()` with `link_no_parent_flush()` to avoid parent without-csum flush inside link()
2. Merge child init flush (#1) with final flush (#3): skip the early `write_back_inode_without_csum`, let the final `write_back_inode` handle it

**Expected**: Each create() goes from ~6 flush to ~2 flush. ino_tbl_w per file drops from 10 → ~5.

**QEMU verify**: stress_create_many profile, 51/51, fast symlink profile unchanged

---

## Phase 3: Coalesce write_at() inode flushes (P2)

**Current**: Each 1KB write triggers inode flush at end of `Ext4OSInode::write_at` (ext4fs.rs:583). Block allocation (balloc.rs:339, extent.rs:842, ext4_inode.rs:984) also triggers immediate inode flush per allocated block.

**Fix**: 
1. Add `metadata_dirty` flag to Ext4OSInode (already exists: `metadata_dirty` AtomicBool)
2. In `write_at()` path, set `metadata_dirty = true` during block allocation instead of flushing
3. Flush once at the end of `write_at()` if `metadata_dirty` is set
4. On close(), flush if `metadata_dirty` (or via existing flush path)

**Risk**: Must ensure size/mtime/ctime are visible to read/lseek after write, even without immediate flush. Cached inode size should handle this.

**Expected**: stress_large_file inode_cache flush drops from 104 → ~25 (mkdir 5 + create 2 + write_at final 1 + 16 block allocs deferred to 1 final = ~25). Per-write flush eliminated.

**QEMU verify**: stress_large_file profile, 51/51, read/write correctness

---

## Phase 4: GD/SB per-syscall coalescing (P3)

**Current**: Every inode bitmap or block bitmap change triggers immediate `sync_bg_to_disk` + `sync_sb_to_disk`.

**Fix**:
1. Add per-operation dirty tracking for superblock and group descriptors
2. At start of syscall, snapshot gd/sb state
3. During allocation/free, accumulate changes in memory (update free_inode_count, free_block_count, bg flags)
4. At end of syscall (or at explicit sync point), write dirty gd/sb once per modified block

**Risk**: Must ensure all changes are flushed before returning to userspace. No async/batching across syscalls.

**Expected**: gd_w drops from 153 → ~10 for create_many (one per modified group descriptor block).

**QEMU verify**: stress_create_many profile, 51/51

---

## Phase 5: Re-enable meta_batch (P4) — deferred

**Known blocker**: `ialloc_alloc_inode` reloads bg from disk within batch, only writes `-1` instead of cumulative `-N`. Need to fix cumulative bg/sb state tracking before re-enabling.

**Do NOT implement now**. Document as known debt.

---

## Acceptance per Phase

Each phase must:
1. Oracle review of code changes ✅
2. `make rv64-kernel-build-only` + `make la64-kernel-build-only` ✅
3. QEMU test: 51/51 FS test ✅
4. read/readlink/read_via_symlink still 0 block I/O ✅
5. single fast symlink profile does not regress ✅
6. Target counter shows expected reduction ✅
