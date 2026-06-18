# ext4 write_at inode lock reentrancy fix evidence

Date: 2026-05-16

## Problem

Latest GDB backtrace showed the active boot hang was not the earlier suspected network/buddy allocator path. The kernel was spinning in `spin::mutex::ticket::TicketMutex<Ext4InodeRef>::lock` through this call chain:

```text
Ext4OSInode::get_new_page_cache
Ext4OSInode::write_at
MountFSInode::write_at
File::write
flush_preload
rust_main
```

`Ext4OSInode::write_at()` refreshed inode metadata while holding `self.inode`, then called `get_new_page_cache()`. `get_new_page_cache()` also locks `self.inode`, causing same-thread reentrancy on a non-reentrant ticket mutex.

## Fix

File: `os/src/fs/ext4/ext4fs.rs`

- Refresh inode metadata inside a short explicit lock scope.
- Release `self.inode` before any PageCache operation.
- Invalidate only an already-existing PageCache through `self.new_page_cache.lock().clone()`.
- Avoid calling `get_new_page_cache()` just to invalidate, so invalidation cannot lazily create cache or re-lock `self.inode`.

## Verification

### rv64 kernel build

Result: PASS

```text
Finished `release` profile [optimized + debuginfo] target(s)
cp -f target/riscv64gc-unknown-none-elf/release/os ../kernel-rv
```

### rv64 QEMU smoke

Result: PASS for this fix scope.

The kernel passed `flush_preload()` and entered `initproc`:

```text
sinitproc: ... sbash: ... ebash: ... sbusybox: ... ebusybox: ...
[fs] found ext4 filesystem
[initproc] installing busybox applets to /bin ...
[initproc] busybox --install -s /bin -> exit=0
[initproc] linking musl/glibc libs to /lib ...
[initproc] lib linking done
[initproc] run_selected_groups start mask=0x001 ...
[initproc] exec failed for basic_testcode.sh in /musl via /bin/bash -c
```

The remaining `basic_testcode.sh` exec failure is the separate out-of-scope issue the user explicitly asked to ignore for now.

### rv64 debugfs image check

Result: PASS for `/bin/bash` and `/bin/busybox` persistence after the smoke run.

```text
debugfs -R 'stat /bin/bash' sdcard-rv.img
Inode: 3219   Type: regular    Mode: 0777   Size: 1147768
EXTENTS:
(0-280):66314-66594

debugfs -R 'stat /bin/busybox' sdcard-rv.img
Inode: 3218   Type: regular    Mode: 0777   Size: 1387560
EXTENTS:
(0-338):65975-66313
```

### la64 kernel build

Result: PASS after generating the missing release `initproc` user artifact.

```text
docker exec -w /app/user oskernel2026-mango-os-dev-1 bash -lc 'make rust-user BOARD=laqemu MODE=release'
Finished `release` profile [optimized] target(s)

kernel-dev_kernel_build(arch="la64", log="off")
Finished `release` profile [optimized + debuginfo] target(s)
cp -f target/loongarch64-unknown-linux-gnu/release/os ../kernel-la
```

The earlier failure was caused by `make la64-only` building userspace in debug mode while the kernel preload incbin expects `../user/target/loongarch64-unknown-linux-gnu/release/initproc`. It was a verification setup issue, not caused by the `write_at()` lock-scope fix.
