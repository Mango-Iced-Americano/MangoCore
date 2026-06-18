# Stage 0 Baseline - LTP FS Target Case Analysis
> Generated: 2026-06-10
> Source: output-rv.txt (2026-06-08 full heaptrace suite run)

## 10 Target Cases - Complete Status

| # | Testcase | Binary Exists | Run Result | Classification | Stage Target | Failure Detail |
|---|----------|:------------:|:----------:|:--------------:|:------------:|----------------|
| 1 | chdir01 | YES (glibc+musl) | TBROK | ENV_FAIL | Stage 2 | Needs LTP_DEV=/dev/vdb2; chdir01A (separate bin) PASSes |
| 2 | chmod05 | YES (glibc+musl) | TFAIL (1) | CURRENT_FAIL | Stage 3 | "Incorrect modes 043777, Expected 041777" — sticky bit SGID propagation |
| 3 | umask01 | YES (glibc+musl) | TFAIL (all) | CURRENT_FAIL | Stage 3 | umask NO-OP: always returns 0, file mode always 0777 |
| 4 | open11 | YES (glibc+musl) | TFAIL (1) | CURRENT_FAIL | Stage 4 | O_CREAT on directory succeeds (should EISDIR); O_CREAT on symlink dir succeeds |
| 5 | linkat01 | YES (glibc+musl) | TFAIL (5) | CURRENT_FAIL | Stage 5 | EBADF unexpected (2x), EISDIR vs EPERM (dir link), symlink succeeds, cleanup TWARN |
| 6 | rename04 | YES (glibc+musl) | TBROK | ENV_FAIL | Stage 6 | Needs LTP_DEV=/dev/vdb2 block device |
| 7 | statx03 | YES (glibc+musl) | TFAIL (2) | CURRENT_FAIL | Stage 7 | "statx() returned with 0" — mask/want bits not properly validated |
| 8 | setxattr01 | YES (glibc+musl) | TBROK | ENV_FAIL | Stage 9 | Needs LTP_DEV=/dev/vdb2 block device |
| 9 | mount02 | YES (glibc+musl) | TBROK | ENV_FAIL | Stage 11 | Needs LTP_DEV=/dev/vdb2 block device |
| 10 | fsync04 | YES (glibc+musl) | TBROK | ENV_FAIL | Stage 8 | Needs LTP_DEV=/dev/vdb2 block device |

## Summary

- **Binaries in image**: 10/10 (100%) — all exist in both glibc and musl
- **CURRENT_FAIL (TFAIL)**: 5 cases — chmod05, umask01, open11, linkat01, statx03
- **ENV_FAIL (TBROK)**: 5 cases — chdir01, rename04, setxattr01, mount02, fsync04
- **NOT_RUN_ENABLE**: All 10 can be run with `ltp_include=X` (bypasses should_skip_ltp_helper)
- **PANIC/TIMEOUT**: None — all cases terminate cleanly

## Critical Observations

1. **5/10 cases are ENV_FAIL** (require LTP_DEV=/dev/vdb2 block device). These affect Stages 2, 6, 8, 9, 11.
2. **chdir01 cannot verify Stage 2.1** (ENOTDIR fix) — it's ENV_FAIL, not CURRENT_FAIL.
3. **chmod05 tests sticky bit mode** not chmod errno — relevant to Stage 3 but different subcase.
4. **statx01 and statx02 PASS** — so statx is partially working; statx03 specifically tests invalid flags.
5. **linkat01 has 22 subcases, 17 PASS** — 5 TFAIL are specific errno issues (EBADF ordering, EISDIR vs EPERM).
