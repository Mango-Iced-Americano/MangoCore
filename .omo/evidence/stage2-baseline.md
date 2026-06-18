# Stage 2 Baseline: Path Resolution LTP Results (2026-06-10)

## Available Binaries
chdir01: NOT_FOUND
lstat02: ✅ (3 TPASS, 3 TFAIL)
pathconf01: ✅ (17 TPASS)
pathconf02: ✅ (4 TPASS, 2 TFAIL)
readlink01: ✅ (2 TPASS)
readlink03: ✅ (7 TPASS, 1 TFAIL)
readlinkat01: ✅ (TBROK: ELOOP on open)
realpath01: ✅ (1 TFAIL)
symlink01: ✅ (5 TPASS)
symlinkat01: ✅ (9 TPASS, 1 TFAIL)

## Root Cause Analysis

### lstat02 TFAILs
1. lstat() returned 0 when should return -1 (2 cases): lsstat not failing on bad paths/invalid operations
2. Expected ENAMETOOLONG got ENOENT: path length check returning wrong errno

### readlink03 TFAIL
1. readlink() succeeded unexpectedly on non-symlink file: Missing FileType check (should return EINVAL)

### readlinkat01 TBROK
- open(readlink_symlink) failed: ELOOP: O_NOFOLLOW behavior issue

### pathconf02 TFAILs
1. Expected EACCES got ENOTDIR: Permission check vs path component type check order
2. Expected ENAMETOOLONG got ENOENT: Path validation order

### realpath01 TFAIL
- realpath(".", NULL) expected ENOENT got SUCCESS: realpath handling of "." is wrong

### symlinkat01 TFAIL
- errno=9 (EBADF) mismatch: Expected different errno for specific scenario

### regression_preserved
readlink01: 2/2 TPASS ✅
symlink01: 5/5 TPASS ✅
pathconf01: 17/17 TPASS ✅
