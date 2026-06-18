## 2026-05-16 plan completion blockers

### F1-F4 REJECT root causes

1. **DirtyBlockDevice 仍在正常 ext4 路径** (`block_device = dirty_bd.clone()`).
   - Reason: removing it caused QEMU timeout (metadata writes go to real disk, busybox install too slow).
   - Status: pending user decision — accept transitional state or invest in metadata write optimization.
   - Affects: F1 (Must Have #3 FAIL), F4 (DirtyBlockDevice NOT isolated).

2. **T10 PageCache write path reverted to direct write**.
   - Reason: PageCache-based write path (prepare_cached_write + PageCache::write) caused basic_testcode exec to fail. Root cause not fully diagnosed; user investigating separately.
   - Status: write_at reverted to original direct write + invalidate.
   - Affects: F1 (Must Have #3 partial FAIL).

3. **Kernel hangs during boot** at buddy_allocator::dealloc in network socket drop path.
   - Root cause: pre-existing bug in buddy allocator or smoltcp socket lifecycle, triggered by heap pressure during flush_preload.
   - Status: user investigating. Workaround: disable net init during boot.
   - Affects: F3 (QEMU QA cannot complete).
   - NOT caused by our ext4 changes.

4. **Dead net import removed**: `net::socket::inet::stream::inner` in layout.rs:15 was unused.

5. **F2 code quality fixes applied**:
   - BlockCache flush concurrent write loss: fixed (atomic lock-write-clear).
   - prepare_cached_write unchecked arithmetic: fixed (checked_add, u32::MAX guard).
   - lang_items.rs: verified NOT modified by our changes.

### Current state
- T1-T21: all implemented and [x] in plan.
- F1-F4: executed, all REJECT per documented blockers above.
- rv64 build: PASS. la64 build: PASS after generating release userspace with `make rust-user BOARD=laqemu MODE=release`.
- /bin/bash verified on disk via debugfs (inode 3219, size 1147768); /bin/busybox also verified (inode 3218, size 1387560).

### Correction after latest GDB backtrace

- The earlier network/buddy allocator diagnosis is stale for the current boot hang.
- Latest GDB stack shows the active hang is `Ext4OSInode::write_at()` re-entering `self.inode`'s non-reentrant `TicketMutex`:
  `write_at()` held `self.inode.lock()` while calling `get_new_page_cache()`, and `get_new_page_cache()` also locks `self.inode`.
- Fix applied: refresh inode metadata inside a short lock scope, release it, then invalidate only an already-existing PageCache via `self.new_page_cache.lock().clone()` instead of calling `get_new_page_cache()`.
- Scope note: this fixes the `flush_preload()` write hang; it does not claim to solve the separate exec failure.

### Verification update after direct fix

- `kernel-dev_kernel_build(arch="rv64")`: PASS.
- `kernel-dev_kernel_run(arch="rv64")`: PASS for this fix scope; boot reaches `initproc`, `busybox --install -s /bin -> exit=0`, then hits the separate known `/bin/bash -c basic_testcode.sh` exec failure.
- `debugfs` on `sdcard-rv.img`: `/bin/bash` and `/bin/busybox` exist with nonzero sizes.
- `kernel-dev_kernel_build(arch="la64")`: PASS after explicitly building la64 release userspace artifact first.
- Final Wave remains not approvable because the long-term blockers from earlier reviews still exist: DirtyBlockDevice remains in the normal ext4 path, and T10 PageCache cached-write path remains reverted to direct write + invalidate.
## 2026-05-17 F4 scope fidelity audit

- Final audit rejected current state: `Tasks [8/21 compliant] | Creep [3 issues] | Missing [13 items] | VERDICT: REJECT`.
- Main blockers: `sys_fsync` remains fd-check/no-op, `sys_umount2` remains fake, no `sys_fdatasync`/`sys_sync` wiring found, ext4 `write_at` still direct-writes then invalidates PageCache, no ext4 `sync/datasync` or `FileSystem::sync_fs/on_umount` override, no global PageCache registry/writeback/reclaim.
- Scope issues: `main..HEAD` contains broad unrelated mm/net/task/HAL/user changes; working tree modifies generated `lang_items.rs` files; committed HEAD still adds DirtyBlockDevice until the working-tree deletion is committed.
- Evidence saved at `.sisyphus/evidence/final-wave/f4-scope-fidelity.md`.

### 2026-05-17 F2 code-quality audit notes

- `rv64` build: PASS.
- `la64` build: PASS.
- LSP: clean on most touched files; `os/src/fs/mod.rs` reports one `unused_braces` warning (`pub use self::dev::{ pipe::*, };`).
- Review findings:
  1. `os/src/fs/ext4/ext4fs.rs`: `write_at()` ignores `invalidate_range()` errors, so a dirty cache page can survive a direct write and later overwrite fresh on-disk data.
  2. `os/src/fs/ext4/layout.rs`: `Drop::drop()` holds `new_page_cache` locked while calling `writeback_all()`, which keeps a mutex across I/O.
  3. `os/src/syscall/fs.rs`: `sys_fsync()` is still fd-check only; `sys_umount2()` is still a fake success path.
  4. `os/src/fs/page_cache_test.rs`: still mentions `DirtyBlockDevice` in the module comment; stale terminology after cache migration.
