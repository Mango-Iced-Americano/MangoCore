# ext4 Metadata Write Amplification Audit

## Data Source

Profiles from QEMU rv64 test run (46/51 tests, 2026-05-18).

## 1. Write Amplification per Operation

### Create (50 empty files — `stress_create_many`)

| Resource | Total (50 files) | Per File | Ideal | Amplification |
|----------|-----------------|----------|-------|---------------|
| block_write_total | 1274 | 25.5 | ~4 | **6.4×** |
| inode table write | 509 | 10.2 | 1 | **10×** |
| inode bitmap write | 102 | 2.0 | 1 | **2×** |
| block bitmap write | 51 | 1.0 | 0 | N/A |
| directory write | 104 | 2.1 | 1 | **2×** |
| group desc write | 153 | 3.1 | 0 | N/A |
| superblock write | 153 | 3.1 | 0 | N/A |
| data write | 50 | 1.0 | 0 | N/A |
| inode cache flush | 305 | 6.1 | 1-2 | **3-6×** |

**Key observations:**
- **509 inode table writes for 50 files** — each file create flushes the inode ~10 times. Root cause: `write_back_inode` called at each step (create inode, link, update size, update mtime, dir entry). No operation-local coalescing.
- **153 gd/sb writes each** — group descriptors and superblock are flushed per-inode despite only free_inode_count changing. These should be coalesced or batched.
- **305 inode cache flushes** — `flush_inode_pagecache_if_dirty` + `write_back_inode` called multiple times per operation. A single close/fsync should suffice.

### Unlink (30 files — `stress_unlink_loop`)

| Resource | Total (30 files) | Per File |
|----------|-----------------|----------|
| block_write_total | 754 | 25.1 |
| inode table write | 300 | 10.0 |
| gd write | 91 | 3.0 |
| sb write | 91 | 3.0 |
| dir write | 61 | 2.0 |
| inode cache flush | 180 | 6.0 |

**Pattern identical to create** — unlink has same amplification.

### Large File (64KB write+read — `large_file_64k`)

| Resource | Count | Notes |
|----------|-------|-------|
| block_write_total | 280 | |
| inode table write | 112 | SUSPICIOUS: 64KB = 16 data blocks, but 112 inode writes |
| inode cache flush | 104 | 104 flushes for 16 data block writes |

**Key finding:** The 64KB write path calls `write_back_inode` on EVERY data block write (size/mtime update), not just once at close. This explains the 104 inode flushes for 16 data blocks.

### Varying Size Write (`write_varying_sizes`)

| Resource | Count |
|----------|-------|
| block_write_total | 80 |
| inode table write | 33 |
| inode cache flush | 25 |

Amplification is proportional to number of `sys_write` calls (each triggers mtime+size update), not to actual data volume.

### Symlink Create (single fast symlink — `create_fast_symlink_1`)

| Resource | Count | Ideal |
|----------|-------|-------|
| block_write_total | ~8 | ~4 |
| inode table write | 2 | 2 |
| symlink_io ino_w | 1 | 1 |
| symlink_io parent_w | 1 | 1 |
| dir_w | 0→1 (fixed in P3) | 1 |

This is already near-optimal (after Phase 5 de-dup in prior work).

## 2. Root Causes of Write Amplification

### 2.1 Inode Table Write ×10

Each `create()` calls:
1. `write_back_inode_without_csum` on child (zero init)
2. `write_back_inode` on parent (link update → size/mtime/links_count)
3. `write_back_inode` on child (final size/mode)
4. `write_back_inode` on parent again (sometimes via flush)

The `write_back_inode` function does a FULL inode write to disk every time, not just the changed fields.

### 2.2 GD/SB Write ×3 per File

Each inode allocation triggers `update_group_desc` → `write_block` for the group descriptor. Each block allocation (including inode table growth) triggers similar updates. The superblock is also re-written because `free_inodes_count` changes.

### 2.3 Inode Cache Flush Excessive

The `flush_inode_pagecache_if_dirty` + `write_back_inode` pattern is called:
- After inode creation
- After link
- After size update
- After mtime update
- During close (fsync)
- During flush_all_page_caches

Each call serializes the entire inode to disk, even when only 1 field changed.

### 2.4 Data Write → Per-Block Inode Flush

64KB write (16 data blocks) triggers 104 inode flushes because:
- Each block allocation updates the extent tree (metadata write)
- Each extent update triggers an inode write_back (mtime/ctime/size)
- Page cache writeback is NOT the cause — these are SYNCHRONOUS inode writes

## 3. Optimization Candidates

### 3.1 Operation-Local Coalescing (low risk, high impact)

**Inode writes:** Within a single syscall (create/write/unlink), accumulate inode changes in memory and flush once at the end.

- `create`: flush child once, flush parent once (currently 10+ flushes)
- `write`: flush inode once at close/fsync, not per-write
- `unlink`: flush parent once, flush freed inode once

**GD writes:** If multiple blocks are allocated in the same group within one operation, coalesce the group descriptor write.

**SB writes:** Superblock changes (free_inode_count, free_block_count) should be written once per operation, not once per inode.

**Expected improvement:**
- create/unlink: 25 blocks → ~6 blocks per file
- 64KB write: 280 blocks → ~40 blocks

### 3.2 Needs Metadata Batch (higher risk, requires batch infrastructure)

- Cross-syscall batching of gd/sb writes
- Dirty inode caching (write back lazily, not synchronously)
- Block bitmap batch updates

These require infrastructure changes (async writeback, batch context) beyond the scope of this audit.

### 3.3 Already Optimized

- `create_fast_symlink` is already coalesced (child + parent each written once)
- `children` cache reduces directory block READS effectively
- `cached_symlink_target` eliminates symlink block reads

## 4. Recommendations

| Priority | Action | Difficulty | Impact |
|----------|--------|-----------|--------|
| P0 | Coalesce inode writes within `create()` and `write_at()` paths | Low | -50% inode writes |
| P1 | Defer gd/sb writes to end of operation | Medium | -70% gd/sb writes |
| P2 | Defer inode metadata writeback in write path (mtime/size coalescing) | Medium | -80% inode flushes for large file |
| P3 | Re-enable meta_batch (requires fixing `ialloc` counting bug) | High | Cross-syscall batching |
| P4 | Dirty inode cache (async writeback) | High | Full reduction |

## 5. Verification

After implementing any optimization:
1. Run `stress_create_many` profile — verify `ino_tbl_w` per file < 5
2. Run `stress_large_file` profile — verify `inode_cache_flush` < 20 for 64KB
3. Confirm `read_via_symlink` still 0 block I/O
4. Confirm `create_fast_symlink_1` profile does not regress
