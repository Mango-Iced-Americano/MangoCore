# Stage 0 - FS LTP Failure Analysis
> Generated: 2026-06-10
> Source: output-rv.txt (2026-06-08 full heaptrace suite run, rv64/glibc)

## Failure Grouping by Family

### Family: chdir (path resolution)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| chdir01 | TBROK | ENV_FAIL | No free devices (LTP_DEV=/dev/vdb2 required) |
| chdir01A | TPASS | PASS | 3/3 subcases pass — symlink chdir works |

### Family: chmod (permission modes)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| chmod05 | TFAIL (1) | CURRENT_FAIL | mode 043777 vs expected 041777 (sticky bit/SGID) |
| fchmod05 | TFAIL (1) | CURRENT_FAIL | same pattern: 041777 vs 043777 |

### Family: umask (file creation mask)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| umask01 | TFAIL (all) | CURRENT_FAIL | umask() returns 0 always (NO-OP), file mode always 0777 |

### Family: open (O_CREAT on dir)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| open11 | TFAIL (1) | CURRENT_FAIL | O_CREAT on directory returns success (should EISDIR) |
| open01-10,12-14 | — | NOT_RUN_ENABLE | Not in this suite run, but binaries exist |

### Family: linkat (hardlink semantics)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| linkat01 | TFAIL (5/22) | CURRENT_FAIL | EBADF unexpected (2), EISDIR vs EPERM (1), symlink link succeeds (1), cleanup TWARN (1) |
| linkat02 | TBROK | ENV_FAIL | No free devices |

### Family: rename (rename semantics)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| rename04 | TBROK | ENV_FAIL | No free devices |

### Family: statx (metadata)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| statx01 | TPASS | PASS | All subcases pass |
| statx02 | TPASS | PASS | All subcases pass |
| statx03 | TFAIL (2) | CURRENT_FAIL | "statx() returned with 0" — mask/want bits issue |

### Family: xattr (extended attributes)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| setxattr01 | TBROK | ENV_FAIL | No free devices |

### Family: mount (filesystem mount)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| mount02 | TBROK | ENV_FAIL | No free devices |

### Family: fsync (sync)
| Testcase | Result | Classification | Notes |
|----------|--------|---------------|-------|
| fsync04 | TBROK | ENV_FAIL | No free devices |

## ENV_FAIL Root Cause
All 5 ENV_FAIL cases fail with the same pattern:
```
tst_device.c:147: TINFO: No free devices found
tst_device.c:354: TBROK: Failed to acquire device
```
The `LTP_DEV=/dev/vdb2` environment variable is set in initproc.rs (line 2339), but the block device `/dev/vdb2` is not available in QEMU without additional setup (second virtio-blk device).

## Summary Statistics
- **Total unique FS TFAIL in full suite**: ~57 (from heaptrace report)
- **Target cases CURRENT_FAIL**: 5 (umask01, chmod05, open11, linkat01, statx03)
- **Target cases ENV_FAIL**: 5 (chdir01, rename04, setxattr01, mount02, fsync04)
- **Target cases PANIC/TIMEOUT**: 0
