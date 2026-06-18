# Wave 1 notes

## 2026-05-16

- Several delegated agents hit model usage limits and fell back repeatedly.
- T1 completed successfully and generated baseline evidence.
- T2/T3/T4 background tasks returned with no assistant/tool output; treat as no-op and parent agent took over.
- T2 parent-agent changes:
  - `File::fsync()` and `File::fdatasync()` added.
  - `FileSystem::on_umount()` now defaults to `sync_fs()` and returns `Result<(), SyscallErr>`.
  - `MountFS::umount()` propagates `on_umount()` errors.
  - rv64/la64 kernel builds pass.
- T3 parent-agent changes:
  - Added global PageCache registry in `os/src/fs/page_cache.rs`.
  - Added `list_page_caches()` and `flush_all_page_caches()`.
  - Registry lock is released before `writeback_all()` to avoid holding a global lock during I/O.
  - rv64/la64 kernel builds pass.
- T4 parent-agent changes:
  - Added `BlockCacheManager::flush_dirty_blocks()` in `os/src/fs/cache.rs`.
  - The method copies dirty buffer data before device writes, avoiding holding individual buffer locks during block I/O.
  - rv64/la64 kernel builds pass.
- T5 parent-agent takeover:
  - Created ownership map for ext4 write paths.
  - Classified DirtyBlockDevice references as remove/quarantine/temporary-dependency.
- T6 parent-agent takeover:
  - Audited fsync/umount2/msync and absent fdatasync/sync/syncfs/sync_file_range.
  - Confirmed fsync and umount2 are silent/fake success and must be fixed by T7/T8.
- T7 parent-agent changes:
  - Added syscall IDs and dispatch for `sync` and `fdatasync`.
  - Rewired `fsync`/`fdatasync` through File/VFS inode sync without fd_table lock across writeback.
  - Added `sys_sync()` using `VFS_ROOT.inner_filesystem().sync_fs()` and `flush_all_page_caches()`.
- T8 parent-agent changes:
  - Replaced fake `umount2` with VFS mountpoint validation and `MountFS::umount()` call.
  - Root unmount returns EBUSY; unsupported flags return EINVAL.
- LSP diagnostics failed because rust-analyzer is unavailable in the configured nightly toolchain.
