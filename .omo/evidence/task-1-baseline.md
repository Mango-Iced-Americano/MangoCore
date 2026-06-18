# Task 1 — Baseline: ext4 Cache/Writeback Path Map

> Status: **Baseline capture — no code changed.**
> Date: 2026-05-16
> Source commit: pre-modification (see `git log -1`)

---

## 1. Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      Userspace (initproc/test)              │
│  write(fd, buf, n)  ●  fsync(fd)  ●  close(fd)             │
└───────────────────┬─────────────────────────────────────────┘
                    │ syscall
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  syscall dispatch (syscall/mod.rs)                          │
│  SYSCALL_WRITE → sys_write → File::write()                  │
│  SYSCALL_FSYNC → sys_fsync (NO-OP, see §5.1)               │
│  SYSCALL_UMOUNT2 → sys_umount2 (FAKE, see §5.2)             │
└───────────────────┬─────────────────────────────────────────┘
                    │ vfs::File::write()
                    ▼
┌─────────────────────────────────────────────────────────────┐
│  Ext4OSInode::write_at()  (IndexNode trait)                 │
│  ① ext4fs.write_at() — bypasses PageCache entirely!        │
│  ② invalidates affected PageCache range                     │
│  ③ refreshes inode metadata from disk                       │
└───────┬─────────────────────────────────────────────────────┘
        │ ③ refresh: fs.get_inode_ref() → reads from device
        │ ① write:   fs.block_device.write_block()
        ▼   (fs.block_device is DirtyBlockDevice)
┌─────────────────────────────────────────────────────────────┐
│  DirtyBlockDevice::write_block(block_id, data)              │
│  → stores data in BTreeMap<usize, Vec<u8>> (memory only)    │
│  → does NOT write to real device                            │
│  → read_block() checks cache first → read-your-writes       │
└─────────────────────┬───────────────────────────────────────┘
                      │ flush_dirty_blocks() → writes to real device
                      │ ONLY called from:
                      │   Ext4FileSystem::sync_fs()
                      │     → NewFileSystem::sync_fs()
                      │       → flush_preload() at boot only
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  Real BlockDevice (virtio-blk / virtio-blk-pci)             │
│  → QEMU virtio device → physical/simulated disk image       │
└─────────────────────────────────────────────────────────────┘
```

**PageCache path** (Ext4OSInode::read_at only, write_at skips it):

```
Ext4OSInode::read_at()
  → PageCache::read()
    → get_or_create_entry() → frame_alloc + backend.read_page()
      → Ext4PageCacheBackend::read_page()
        → fs.block_device.read_block()  → DirtyBlockDevice.read_block()
```

**PageCache writeback path** (only on Drop):

```
Ext4OSInode::drop()
  → PageCache::writeback_all()
    → Ext4PageCacheBackend::write_page()
      → fs.block_device.write_block() → DirtyBlockDevice.write_block()  [still cached!]
```

---

## 2. Cache Layer Inventory

### 2.1 DirtyBlockDevice — `os/src/fs/ext4/dirty_block_device.rs` (78 lines)

| Method | Behavior |
|--------|----------|
| `new(inner)` | Wraps real `Arc<dyn BlockDevice>`, initializes empty `Mutex<BTreeMap<usize, Vec<u8>>>` |
| `read_block(id, buf)` | **Cache-first:** checks dirty map; if hit, copies cached bytes into buf. Miss → `inner.read_block()`. |
| `write_block(id, buf)` | **Memory-only:** inserts `(id, buf.to_vec())` into dirty map. No real device write. |
| `clear_block(id, num)` | Inserts zeroed block into dirty map (memory only). |
| `clear_mult_block(id, cnt, num)` | Same, for range. |
| `flush_dirty_blocks()` | **Takes** the entire dirty map, iterates, calls `inner.write_block()` for each. Clears map. |

**Key property:** Read-your-writes consistency within a single boot. The dirty map ensures `read_block()` after `write_block()` returns the new data. But **no data reaches disk unless `flush_dirty_blocks()` is called**.

### 2.2 Ext4FileSystem fields — `os/src/fs/ext4/ext4fs.rs:23-36`

```rust
pub struct Ext4FileSystem {
    pub block_device: Arc<dyn BlockDevice>,  // = dirty_bd (the DirtyBlockDevice wrapper)
    dirty_bd: Arc<DirtyBlockDevice>,           // same Arc, separate handle
    pub superblock: SuperBlock,
    pub block_size: usize,
    pub cache_mgr: Arc<Mutex<BlockCacheManager>>,
    __self_ref: spin::Mutex<Weak<Ext4FileSystem>>,
}
```

`block_device` and `dirty_bd` wrap the **same** `DirtyBlockDevice` instance. All `Ext4FileSystem` methods that call `self.block_device.write_block()` are writing into the dirty cache, not to disk.

### 2.3 BlockCacheManager — `os/src/fs/cache.rs:101-191`

Old metadata cache (BufferCache pool). Used mostly by legacy fat32 code and some ext4 metadata paths (block groups, bitmaps).

| Method | Behavior |
|--------|----------|
| `get_block_cache(id, dev)` | Lookup or allocate + read from real device. |
| `oom(dev)` | Eviction: if `priority == 0` and `strong_count == 1`, writes dirty buffer to **real device**. |
| `alloc_buffer_cache(dev)` | Loops: find free slot, else `oom()`. |

**Note:** `BlockCacheManager` writes **directly to the real device** (via the `block_device` argument passed in). This is **not** wrapped in `DirtyBlockDevice`. Only `Ext4FileSystem.block_device` is wrapped. Legacy buffer cache paths bypass DirtyBlockDevice.

### 2.4 PageCache state machine — `os/src/fs/page_cache.rs:111-509`

```
PageState transitions:
  Loading → UpToDate ←→ Dirty → Writeback → UpToDate
                          ↓
                        Error (on backend I/O failure)
```

| State | Meaning |
|-------|---------|
| `Loading` | Initial state on allocation |
| `UpToDate` | Contents match backend (or never dirtied) |
| `Dirty` | Modified in memory, not yet written back |
| `Writeback` | Writeback in progress |
| `Error` | Backend I/O failure |

Key method chain:
- `writeback_page(idx)` → sets `Writeback`, calls `backend.write_page()`, sets `UpToDate`
- `writeback_all()` → collects dirty indices, calls `writeback_page()` for each
- `writeback_range(start, end)` → same, for range

### 2.5 Ext4PageCacheBackend — `os/src/fs/page_cache.rs:712-791`

```rust
struct Ext4PageCacheBackend {
    ext4fs: Weak<Ext4FileSystem>,
    inode_num: u32,
    block_size: usize,
    blocks_per_page: usize,
}
```

| Method | Behavior |
|--------|----------|
| `read_page(idx, buf)` | For each `block_off`: `block_id_for_offset()` → `fs.block_device.read_block()`. Holes → fill zero. |
| `write_page(idx, buf)` | For each `block_off`: `block_id_for_offset()` → `fs.block_device.write_block()`. |

**Critical:** `fs.block_device` is `DirtyBlockDevice`. So `write_page()` writes to DirtyBlockDevice's **memory cache**, not to the real device. The PageCache writeback merely moves data from PageCache frames → DirtyBlockDevice dirty map.

### 2.6 Ext4OSInode drop writeback — `os/src/fs/ext4/layout.rs:82-88`

```rust
impl Drop for Ext4OSInode {
    fn drop(&mut self) {
        if let Some(ref pc) = *self.new_page_cache.lock() {
            let _ = pc.writeback_all();
        }
    }
}
```

Writeback happens ONLY when the `Arc<Ext4OSInode>` is dropped. This moves PageCache dirty pages → DirtyBlockDevice dirty map. **Still not on disk.**

---

## 3. Writeback Call Chain

### 3.1 Data writes (sys_write → write_at)

```
sys_write(fd, buf, n)             syscall/fs.rs
  → vfs::File::write(buf)          fs/vfs/file.rs:562
    → Ext4OSInode::write_at()      fs/ext4/ext4fs.rs:411
      → ext4fs.write_at()           ext4fs internal
        → block_device.write_block()  → DirtyBlockDevice (memory)
      → PageCache::invalidate_range() — evicts cached pages
      → ext4fs.get_inode_ref()       — re-reads inode metadata
```

**The ext4fs.write_at() path (file.rs or ext4fs.rs internal) calls `self.block_device.write_block()` directly, which is DirtyBlockDevice** — this means every ext4 data write goes to memory only. The PageCache is **invalidated** after each write_at(), preventing it from being used as a writeback source for data.

### 3.2 PageCache writeback (Ext4OSInode drop)

```
Ext4OSInode::drop()                layout.rs:82
  → PageCache::writeback_all()     page_cache.rs:420
    → for each dirty index:
      → PageCache::writeback_page(idx)  page_cache.rs:379
        → backend.write_page(idx, data) Ext4PageCacheBackend::write_page()
          → fs.block_device.write_block()  → DirtyBlockDevice (memory again!)
```

### 3.3 DirtyBlockDevice → real device

```
Ext4FileSystem::flush_dirty_blocks()    ext4fs.rs:64
  → dirty_bd.flush_dirty_blocks()       dirty_block_device.rs:35
    → for each (block_id, data) in dirty map:
      → inner.write_block(block_id, data)  → REAL BLOCK DEVICE
```

### 3.4 Who calls flush_dirty_blocks()?

```
NewFileSystem::sync_fs()            ext4fs.rs:754
  → self.flush_dirty_blocks()

Called from:
  fs/mod.rs:516  — flush_preload() at kernel boot
```

**That's it.** `sync_fs()` (and thus `flush_dirty_blocks()`) is called exactly once: at the end of `flush_preload()` during kernel startup, to persist the embedded ELF binaries (initproc, bash, busybox, etc.) that were written at init time.

---

## 4. Syscall Dispatch Table

### 4.1 `os/src/syscall/syscall_id.rs`

| ID | Constant | Value |
|----|----------|-------|
| 39 | `SYSCALL_UMOUNT2` | 39 |
| 82 | `SYSCALL_FSYNC` | 82 |
| 227 | `SYSCALL_MSYNC` | 227 |

No `SYNC` (sync_file_range = 84, sync = 162) is defined. There is no `sync` syscall handler.

### 4.2 `os/src/syscall/mod.rs` dispatch

```
SYSCALL_FSYNC   → sys_fsync(args[0])              :243
SYSCALL_UMOUNT2 → sys_umount2(args[0] as *const u8, args[1] as u32)  :191
SYSCALL_MSYNC   → (not found in dispatch — msync not implemented)
```

No `SYSCALL_SYNC` (162) in the dispatch table at all.

---

## 5. Key Bugs/Holes

### 5.1 `sys_fsync` — `os/src/syscall/fs.rs:1205-1214`

```rust
pub fn sys_fsync(fd: usize) -> isize {
    let task = current_task().unwrap();
    info!("[sys_fsync] fd: {}", fd);
    let fd_table = task.files.lock();
    match fd_table.get_file(fd) {
        Ok(_) => SUCCESS,
        Err(e) => return -(e as isize),
    }
}
```

**BUG:** It only validates the fd exists. It does NOT:
- Call `flush_dirty_blocks()` (doesn't even have access to filesystem)
- Call `sync_fs()` on the filesystem
- Flush the specific file's PageCache
- Any actual persistence operation

This is a **complete no-op** for persistence.

### 5.2 `sys_umount2` — `os/src/syscall/fs.rs:1425-1441`

```rust
pub fn sys_umount2(target: *const u8, flags: u32) -> isize {
    // ... string translation, flag parsing ...
    warn!("[sys_umount2] fake implementation!");
    SUCCESS
}
```

**BUG:** Fake implementation. Does not:
- Flush dirty blocks
- Sync filesystem  
- Detach mount
- Free resources

### 5.3 `sys_msync` — **not implemented at all**

Defined in `syscall_id.rs` as `SYSCALL_MSYNC = 227` but **no dispatch entry** in `syscall/mod.rs`. Will return `ENOSYS` (the default catch-all).

### 5.4 PageCache writeback → still cached

When `Ext4OSInode::drop()` calls `writeback_all()`, the backend writes to `DirtyBlockDevice`, **not** to the real device. So even on inode close/drop, data stays in memory.

---

## 6. Fresh-Image QEMU Procedure

### 6.1 Common prerequisites

```bash
# Enter Docker container
make docker
# or manually:
docker compose up -d
docker compose exec -it os-dev bash
```

### 6.2 Image preparation (both architectures)

```bash
# Fresh image extraction from compressed baseline
xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img   # rv64
xz -dkc fs-img-dir/sdcard-la.img.xz > sdcard-la.img   # la64

# Verify fresh images are new (not previously modified)
md5sum sdcard-rv.img sdcard-la.img

# Keep a pristine copy for re-roll
cp sdcard-rv.img sdcard-rv.img.pristine
cp sdcard-la.img sdcard-la.img.pristine

# Revert to pristine after test
cp sdcard-rv.img.pristine sdcard-rv.img
cp sdcard-la.img.pristine sdcard-la.img
```

### 6.3 offline debugfs inspection

```bash
# Stat an inode to verify contents (before QEMU)
debugfs -R 'stat /bin/bash' sdcard-rv.img
debugfs -R 'stat /initproc' sdcard-rv.img

# Dump a file's contents to host
debugfs -R 'cat /bin/bash' sdcard-rv.img > bash-from-image

# List directory contents
debugfs -R 'ls -l /bin/' sdcard-rv.img

# Check superblock info
debugfs -R 'show_super_stats' sdcard-rv.img

# Dump a block to hex
debugfs -R 'dump_block 100' sdcard-la.img | xxd
```

### 6.4 rv64 QEMU (fresh image)

**Inside Docker**, from project root:

```bash
# Full boot with fresh sdcard-rv.img
timeout 120 \
qemu-system-riscv64 \
  -machine virt \
  -kernel kernel-rv \
  -m 1024 \
  -nographic \
  -smp 1 \
  -bios default \
  -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -no-reboot \
  -rtc base=utc \
  -device virtio-net-device,netdev=net \
  -netdev user,id=net
```

**Direct via Makefile:**

```bash
# From os/ directory
make rv64-only           # builds kernel + user + generates sdcard-rv.img
# or just run with existing kernel:
cd os && make comp       # build+run via rv64.mk comp target
```

### 6.5 la64 QEMU (fresh image)

**Inside Docker**, from project root:

```bash
# Full boot with fresh sdcard-la.img
timeout 120 \
qemu-system-loongarch64 \
  -machine virt \
  -kernel kernel-la \
  -m 1G \
  -nographic \
  -smp 1 \
  -drive file=sdcard-la.img,if=none,format=raw,id=x0 \
  -device virtio-blk-pci,drive=x0 \
  -no-reboot \
  -device virtio-net-pci,netdev=net0 \
  -netdev user,id=net0 \
  -rtc base=utc
```

**Direct via Makefile:**

```bash
# From os/ directory
make la64-only           # builds kernel + user + generates sdcard-la.img
# or just run with existing kernel:
cd os && make comp       # build+run via la64.mk comp target
```

### 6.6 Persistence verification procedure

To test whether a write survives reboot:

```
1. Take fresh image:
   xz -dkc fs-img-dir/sdcard-rv.img.xz > sdcard-rv.img
   md5sum sdcard-rv.img > boot-1-before.md5

2. Boot QEMU, write a file from within the OS:
   echo "hello persistence" > /tmp/test.txt
   (or via test binary: write file, call fsync)

3. Reboot the kernel (or use -no-reboot and let it exit)

4. AFTER QEMU exits, inspect offline with debugfs:
   debugfs -R 'cat /tmp/test.txt' sdcard-rv.img

5. If file contents are present → persistence confirmed.
   If file does not exist or is empty → data was only in-memory.
```

**Important:** A single boot where the file appears in userspace is NOT persistence proof. The DirtyBlockDevice gives read-your-writes consistency within the same boot. You must **reboot** or use **debugfs** on the extracted image **after QEMU exits**.

---

## 7. Distinct Cache Coherency Properties

| Scope | Visibility | Duration | Mechanism |
|-------|-----------|----------|-----------|
| **PageCache** (per-inode) | Current process (shared by fds to same inode) | Until evicted or invalidated | `Arc<PageEntry>` in `Vec<Option<Arc<PageEntry>>>` |
| **DirtyBlockDevice** (per-filesystem) | All reads via same `Ext4FileSystem` | Until `flush_dirty_blocks()` called | `BTreeMap<usize, Vec<u8>>` in `Mutex` |
| **Real BlockDevice** (disk) | All processes, all boots, offline debugfs | Permanent | Physical sectors on virtio device |

**Current state: data in DirtyBlockDevice does NOT go to disk unless `flush_dirty_blocks()` is called, and that function is called only once at boot (from `flush_preload()`).**

---

## 8. Summary of Issues

| Issue | Impact | Root Cause |
|-------|--------|------------|
| `sys_fsync` no-op | Userspace fsync() is a no-op; data loss on crash | Only validates fd, no flush |
| `sys_umount2` fake | Unmount does not flush or clean up | Fake stub returns SUCCESS |
| No `sync` syscall | Userspace cannot trigger global sync | Constant not defined or dispatched |
| PageCache writeback → DirtyBlockDevice | Inode drop flushes PageCache but data stays in DirtyBlockDevice cache | Backend writes to DirtyBlockDevice, not real device |
| DirtyBlockDevice flushed only at boot | Data written during normal operation (sys_write) NEVER reaches disk | `flush_dirty_blocks()` only called from `flush_preload()` |
| `Ext4OSInode::write_at()` bypasses PageCache | PageCache not used for write path; data written directly to DirtyBlockDevice | `write_at()` calls `ext4fs.write_at()` then invalidates PageCache pages |
| BlockCacheManager writes directly to real device | Old metadata cache bypasses DirtyBlockDevice, but no coherency guarantee with the dirty cache | BCM is passed the real `BLOCK_DEVICE`, not the DirtyBlockDevice wrapper |

---

## Appendix A: File Reference Table

| File | Lines | Role |
|------|-------|------|
| `os/src/fs/ext4/dirty_block_device.rs` | 1-78 | Deferred-write `BlockDevice` wrapper |
| `os/src/fs/ext4/ext4fs.rs:23-36` | 14 | `Ext4FileSystem` struct fields |
| `os/src/fs/ext4/ext4fs.rs:64-66` | 3 | `flush_dirty_blocks()` delegate |
| `os/src/fs/ext4/ext4fs.rs:382-434` | 53 | `read_at`/`write_at` IndexNode impl |
| `os/src/fs/ext4/ext4fs.rs:706-757` | 52 | `NewFileSystem` trait impl |
| `os/src/fs/ext4/layout.rs:53-117` | 65 | `Ext4OSInode` struct, drop, `get_new_page_cache` |
| `os/src/fs/page_cache.rs:111-145` | 35 | `InnerPageCache` state machine |
| `os/src/fs/page_cache.rs:149-509` | 361 | `PageCache` full impl |
| `os/src/fs/page_cache.rs:712-791` | 80 | `Ext4PageCacheBackend` |
| `os/src/fs/cache.rs:46-191` | 146 | `BufferCache`/`BlockCacheManager` |
| `os/src/fs/mod.rs:47-123` | 77 | `VFS_ROOT` init |
| `os/src/fs/mod.rs:382-517` | 136 | `flush_preload()` |
| `os/src/syscall/fs.rs:1205-1214` | 10 | `sys_fsync` no-op |
| `os/src/syscall/fs.rs:1425-1441` | 17 | `sys_umount2` fake |
| `os/src/syscall/mod.rs:191-243` | 53 | Dispatch entries |
| `os/src/syscall/syscall_id.rs` | 119 | Syscall ID constants |

## Appendix B: Docker exec variant

If running from host (outside Docker):

```bash
# Execute QEMU inside running container
docker exec oskernel2026-mango-os-dev-1 bash -c "cd /root/projects/oskernel2026-mango && qemu-system-riscv64 -machine virt -kernel kernel-rv -m 1024 -nographic -smp 1 -bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -no-reboot -rtc base=utc -device virtio-net-device,netdev=net -netdev user,id=net"
```

Or for debugfs inspection from host:

```bash
docker exec oskernel2026-mango-os-dev-1 debugfs -R 'stat /bin/bash' /root/projects/oskernel2026-mango/sdcard-rv.img
```
