# Stage 0 Baseline — LTP FS Test Results

## Test Run: 2026-06-10
Arch: rv64, libc: glibc, runner: inline

### Target Cases Results

| Case | Status | Root Cause | Layer |
|------|--------|------------|-------|
| umask01 | TFAIL (1021/1024) | umask always returns 0 (NO-OP); file mode always 777 | syscall |

### Details

#### umask01
- Expected: `umask(mask)` returns previous mask; created file mode = requested & ~mask
- Actual: `umask()` always returns 0; file mode always 0777 regardless of mask
- Root Cause: `sys_umask()` in `os/src/syscall/fs.rs:4453` is a NO-OP
- Fix Stage: Stage 3 (task 3.1)

**Note**: Other target cases (chdir01, chmod05, open11, linkat01, rename04, statx03, setxattr01, mount02, fsync04) may not exist as binaries in the test image. Need to verify availability.

### Regression Set (~50 TPASS)
Full regression set needs to be cataloged from existing status doc.
