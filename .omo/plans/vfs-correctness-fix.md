# VFS Correctness Regression Fix Plan

## Summary

Fix 5 failing FS tests (ELOOP, symlink chain, getdents64, stress unlink empty dir, stress getdents counts) caused by two bugs:
1. **symlink**: `vfs_lookup()` uses broken `absolute_path()` branch for relative symlink targets
2. **getdents64**: `Ext4OSInode` missing `list()` implementation

## Phase 1: Fix symlink path resolution (critical)

### Root Cause

`os/src/fs/mod.rs` `vfs_lookup()` lines 250-264: when encountering a relative symlink target, the code calls `current.absolute_path()` to build an absolute path and restarts from root. But `MountFSInode::absolute_path()` at `mount.rs:417` relies on `parent.inner_inode.get_entry_name(ino_id)` — Ext4OSInode does NOT implement `get_entry_name()`, so the fallback `unwrap_or_else(|_| "?")` produces `"/?"`. The resulting bogus path `/?/loop` fails with ENOENT.

Evidence: Profile shows `readlink=1, symlink_target_hit=1` — meaning `read_at()` succeeded once but was never called again. The failure happens AFTER the first symlink target read, during the redirect path construction.

### Fix

In `os/src/fs/mod.rs` lines 240-272, delete the `else if let Ok(cur_abs) = current.absolute_path()` block (lines 250-264). Keep only:

1. Lines 240-249: Absolute symlink targets (`starts_with('/')`) → restart from root (correct)
2. Lines 265-272: Relative symlink targets → parse `new_path` relative to symlink's parent directory (the correct POSIX semantics, currently unreachable)

**Before** (broken):
```rust
if new_path.starts_with('/') {
    // absolute: restart from root ✓
    ...
} else if let Ok(cur_abs) = current.absolute_path() {
    // relative + absolute_path: BROKEN — builds bogus path
    let look_path = format!("{}/{}", cur_abs, new_path);
    // ... restart from root with bogus path → ENOENT
} else {
    // relative + no absolute_path: CORRECT but never reached
    components = parse_path(&new_path);
    comp_idx = 0;
    continue;
}
```

**After** (fixed):
```rust
if new_path.starts_with('/') {
    // absolute: restart from root ✓
    ...
} else {
    // relative: parse relative to symlink's parent directory ✓
    components = parse_path(&new_path);
    // current stays as symlink's parent directory
    comp_idx = 0;
    continue;
}
```

### Expected Behavior After Fix

- **Self-loop** (`/tmp3/loop → "loop"`): resets `components = ["loop"]`, keeps `current` as `/tmp3`, finds "loop" → symlink → `symlink_count++` → repeat → hits 40 → returns `-ELOOP` ✅
- **Chain** (`/tmp4/c → "b" → "a"`): follows `b` relative to `/tmp4`, then `a` relative to `/tmp4`, opens file → reads "chain-test\n" ✅
- **Readlink** (test 8): unaffected — `readlinkat` uses `vfs_lookup(follow_final=false)` which returns symlink inode directly ✅
- **Read via symlink** (test 9): unaffected — single-level symlink follow continues to work ✅
- **Dangling symlink**: relative target "nonexistent" parsed from parent dir → `find()` returns ENOENT ✅

### Verification

1. `make rv64-kernel-build-only` ✅
2. `make la64-kernel-build-only` ✅
3. QEMU test with mask=0x003 (basic+busybox) — boot + basic tests pass ✅
4. FS test — ELOOP test (9/51) and symlink chain test (10/51) PASS ✅

---

## Phase 2: Fix getdents64 returning ENOSYS

### Root Cause

`Ext4OSInode` in `os/src/fs/ext4/ext4fs.rs` does NOT implement `fn list()` from the `IndexNode` trait. The trait default at `os/src/fs/vfs/index_node.rs:132-133` returns `Err(SyscallErr::ENOSYS)`.

Dispatch chain:
```
sys_getdents64 → File::get_dirent() → IndexNode::list() → default: ENOSYS
```

### Fix

Add `fn list()` to `impl IndexNode for layout::Ext4OSInode` block in `os/src/fs/ext4/ext4fs.rs` (before closing `}` at line 963):

```rust
fn list(&self) -> Result<Vec<String>, SyscallErr> {
    let inode_num = self.inode.lock().inode_num;
    let entries = self.ext4fs
        .dir_get_entries(inode_num)
        .map_err(|_| SyscallErr::EIO)?;
    Ok(entries.iter().map(|e| e.get_name()).collect())
}
```

### Expected Behavior

- getdents64 on ext4 directory returns actual directory entries from disk ✅
- Entries include "." and ".." (ext4 directory format guarantees these) ✅
- Deleted entries (unlinked files) not returned (ext4 marks them with inode=0) ✅
- `File::get_dirent()` calls `find()` for each entry to get d_ino and d_type ✅

### Verification

1. `make rv64-kernel-build-only` + `make la64-kernel-build-only`
2. QEMU FS test:
   - [21/51] getdents64 PASS
   - [45/51] stress unlink empty dir PASS
   - [48/51] stress getdents counts 20 files PASS

---

## Phase 3: Profile classification completeness (minor)

### Tasks

1. Add `symlink_io.dir_w` counter increment in ext4's `dir_add_entry` when called for symlink creation path
2. Add `readdir_dir_block_read` counter increment inside the new `list()` method
3. Audit remaining `write_block` call sites for unclassified writes, add `other_meta_write` where needed

### Verification

- `stress_create_many` profile: `unclassified_write_total` decreases
- `symlink_io.dir_w > 0` in single-symlink tests

---

## Phase 4: Full verification

1. Dual-arch build: `make rv64-kernel-build-only` + `make la64-kernel-build-only`
2. QEMU basic tests (mask=0x001) — no panic, all basic tests pass
3. QEMU FS tests (run all 51) — 51/51 PASS
4. Profile review: read/readlink/read_via_symlink still 0 block I/O

---

## Phase 5: Performance audit (read-only, no code changes)

Analyze profiles after correctness is restored:

1. Per-operation metadata write amplification (ino_tbl_w per create/unlink)
2. Inode cache flush frequency analysis
3. gd/sb write patterns
4. Recommendations: operation-local coalescing candidates vs. metadata batch requirements

---

## Phase 6: Cache lifecycle verification

1. Add `dump_ext4_cache_memory_profile(label)` showing all cache sizes
2. Verify prune/cleanup end-to-end cycle:
   - Create → unlink → drop fds → prune → verify stale=0, dirty=0
3. Verify inode_cache soft cap enforcement
4. Verify symlink_target cache cleanup on unlink
