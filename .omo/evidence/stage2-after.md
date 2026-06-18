## Stage 2 Results — 2026-06-10 (Final)

### Build
- rv64: ✅ (164 pre-existing warnings)
- la64: ✅

### Task Completion

| Task | Status | Notes |
|------|--------|-------|
| 2.1 chdir→ENOTDIR | ✅ | sys_chdir checks FileType::Dir |
| 2.2 middle→ENOTDIR | ✅ | vfs_lookup rejects non-dir intermediate |
| 2.3 ENOENT priority | ✅ | validate_path_len added to fstatat, statx, statfs |
| 2.4 bad-fd→EBADF | ✅ | resolve_start_inode already correct |
| 2.5 trailing→ENOTDIR | ✅ | has_trailing_slash + post-resolution check |
| 2.6 lstat symlink | ⚠️ 4P/2F | EACCES → Stage 3 (permission checks) |
| 2.7 readlink EINVAL | ❓ KNOWN_GAP | Code correct but LTP fails. Needs QEMU debug. |
| 2.8 readlinkat fd | ✅ | resolve_start_inode + vfs_lookup already correct |

### Oracle-Requested Fixes (2026-06-10)
- Removed debug println! in sys_readlinkat → changed to debug!
- Fixed AT_EMPTY_PATH: empty-path check moved after flags parsing
- la64 build PASS added

### Regression Preserved
readlink01(2/2) ✅ symlink01(5/5) ✅ pathconf01(17/17) ✅
