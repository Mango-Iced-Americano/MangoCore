## Stage 0 - Baseline Analysis (2026-06-10)

### Key Findings

#### 10 Target Binary Existence
ALL 10 target binaries exist in the sdcard-rv.img for BOTH glibc and musl:
- `/glibc/ltp/testcases/bin/` and `/musl/ltp/testcases/bin/`: chdir01, chmod05, umask01, open11, linkat01, rename04, statx03, setxattr01, mount02, fsync04
- Binaries were present since image creation (2025-06-14)

#### Why Old Status Doc Says NOT_RUN
The old `Doc/ltp/ltp_fs_status.md` (last updated 2026-05-25) shows all 10 as NOT_RUN because:
1. The inline runner (`should_skip_ltp_helper` in initproc.rs) explicitly skips them
2. The skip list entries: chdir01→"requires LTP external block device", chmod05/umask01→"permission/umask semantics", others caught by prefix skips (statx, setxattr, sync)
3. When using `ltp_include=X` (non-empty include list), the skip helper is BYPASSED (initproc.rs:1453 `if include.is_empty()`)

#### Actual Run Results (from 2026-06-08 full heaptrace run)
| Case | Result | Classification | Root Cause |
|------|--------|---------------|------------|
| chdir01 | TBROK | ENV_FAIL | Needs LTP_DEV=/dev/vdb2 block device |
| chmod05 | TFAIL (1) | CURRENT_FAIL | Sticky bit mode propagation: got 043777, expected 041777 |
| umask01 | TFAIL (all) | CURRENT_FAIL | umask NO-OP: returns 0 always, file mode always 777 |
| open11 | TFAIL (1) | CURRENT_FAIL | O_CREAT on directory succeeds (should be EISDIR) |
| linkat01 | TFAIL (5) | CURRENT_FAIL | Multiple: EBADF unexpected, EISDIR vs EPERM, symlink succeeds |
| rename04 | TBROK | ENV_FAIL | Needs LTP_DEV=/dev/vdb2 block device |
| statx03 | TFAIL (2) | CURRENT_FAIL | statx returns 0 when should fail (mask handling) |
| setxattr01 | TBROK | ENV_FAIL | Needs LTP_DEV=/dev/vdb2 block device |
| mount02 | TBROK | ENV_FAIL | Needs LTP_DEV=/dev/vdb2 block device |
| fsync04 | TBROK | ENV_FAIL | Needs LTP_DEV=/dev/vdb2 block device |

#### Critical Correction vs Plan Assumptions
- **chdir01** is ENV_FAIL (needs block device), NOT a valid test for ENOTDIR fix in Stage 2
- **rename04**, **setxattr01**, **mount02**, **fsync04** are also ENV_FAIL (need block device)
- Only **5 of 10** cases are CURRENT_FAIL (actual kernel bugs): chmod05, umask01, open11, linkat01, statx03
- The ENV_FAIL cases need `LTP_DEV=/dev/vdb2` setup before they can be used as acceptance tests

#### Existing os_test.conf
Already configured with `ltp_include=chdir01,chmod05,umask01,open11,linkat01,rename04,statx03,setxattr01,mount02,fsync04` using `ltp_runner=inline` and `ltp_libc=glibc`.
