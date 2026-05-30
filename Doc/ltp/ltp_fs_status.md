# FS-LTP Testcase 状态表

> 最后更新: 2026-05-25
> 当前阶段: Round A ✅ → Round B ✅ → Round C 收尾
> Oracle 审查: Round A (2026-05-25) + Round B (2026-05-25) 通过

## 字段说明

| 字段 | 说明 |
|------|------|
| **Testcase** | LTP 测例名 |
| **Round** | 所属 FS Round (0/1/2/3) |
| **Family** | 所属 syscall family |
| **运行结果** | TPASS / TFAIL / TBROK / TCONF / PANIC / TIMEOUT / NOT_RUN / NO_BIN |
| **行动分类** | PASS / FIXABLE_NOW / FIXABLE_LATER / UNSUPPORTED / ENV_FAIL |
| **回归集** | YES / NO |

> **NO_BIN** = 该二进制在镜像中不存在（非 inode/文件系统问题，是测试镜像未包含）

---

## 三列表概览

| 列表 | 当前数量 | 说明 |
|------|----------|------|
| **回归集** | ~50 | 已验证稳定通过，每次修复后回归 |
| **可用二进制** | ~360 | 镜像中存在的 FS 相关 LTP 二进制 |
| **强制排除集** | ~200+ | UNSUPPORTED/DANGEROUS_STRESS/FIXABLE_LATER |

---

## Round-0 核心 Family (VFS/fd/path/基础读写)

### open / openat (18 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| open01 | NOT_RUN | FIXABLE_LATER | NO | sticky bit 权限模型 |
| open02 | NOT_RUN | — | NO | |
| open03 | TPASS ✅ | PASS | YES | |
| open04 | TPASS ✅ | PASS | YES | |
| open06 | NOT_RUN | — | NO | |
| open07 | NOT_RUN | — | NO | |
| open08 | NOT_RUN | — | NO | |
| open09 | NOT_RUN | — | NO | |
| open10 | NOT_RUN | — | NO | |
| open11 | NOT_RUN | — | NO | |
| open12 | NOT_RUN | — | NO | 需 open12_child |
| open13 | NOT_RUN | — | NO | |
| open14 | NOT_RUN | — | NO | |
| openat01 | NOT_RUN | — | NO | |
| openat02 | NOT_RUN | — | NO | 需 openat02_child |
| openat03 | NOT_RUN | — | NO | |
| openat04 | NOT_RUN | — | NO | |
| openat201-203 | — | UNSUPPORTED | NO | Linux 5.6+ |

### close (4 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| close01 | TPASS ✅ | PASS | YES | |
| close02 | TPASS ✅ | PASS | YES | |
| close_range01-02 | — | UNSUPPORTED | NO | Linux 5.9+ |

### read (4 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| read01 | TPASS ✅ | PASS | YES | |
| read02 | TPASS ✅ | PASS | YES | |
| read03 | NOT_RUN | — | NO | |
| read04 | NOT_RUN | — | NO | |

### write (6 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| write01 | TPASS ✅ | PASS | YES | |
| write02 | TPASS ✅ | PASS | YES | |
| write03 | NOT_RUN | — | NO | |
| write04 | NOT_RUN | — | NO | |
| write05 | NOT_RUN | — | NO | |
| write06 | TPASS (2/2) ✅ | PASS | YES | Round A: O_APPEND offset 修复 |

### lseek (4 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| lseek01 | TPASS ✅ | PASS | YES | SEEK_SET/CUR/END |
| lseek02 | NOT_RUN | — | NO | |
| lseek07 | NOT_RUN | — | NO | |
| lseek11 | — | UNSUPPORTED | NO | SEEK_DATA/SEEK_HOLE |

### stat / fstat / lstat (12 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| stat01 | TFAIL (2/12) | FIXABLE_LATER | NO | uid=0 vs nobody(65534), 缺 seteuid |
| stat02 | TPASS ✅ | PASS | YES | |
| stat03 | NOT_RUN | — | NO | |
| fstat02 | TPASS ✅ | PASS | YES | |
| fstat03 | NOT_RUN | — | NO | |
| fstatat01 | NOT_RUN | — | NO | |
| lstat01 | NOT_RUN | — | NO | |
| lstat02 | TPASS ✅ | PASS | YES | |
| statx01-12 | — | UNSUPPORTED | NO | Linux 4.11+ |

### access / faccessat (6 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| access01 | TPASS ✅ | PASS | YES | |
| access02-04 | NOT_RUN | — | NO | |
| faccessat01-02 | NOT_RUN | — | NO | |
| faccessat201-202 | — | UNSUPPORTED | NO | Linux 5.8+ |

### getcwd / chdir / fchdir (7 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| getcwd01 | TPASS ✅ | PASS | YES | ERANGE 顺序已修复 |
| getcwd02-04 | NOT_RUN | — | NO | |
| chdir01 | NOT_RUN | — | NO | |
| chdir04 | TPASS ✅ | PASS | YES | ENAMETOOLONG 已修复 |
| fchdir01-03 | NOT_RUN | — | NO | |

### getdents / readdir (4 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| getdents01 | TPASS ✅ | PASS | YES | |
| getdents02 | NOT_RUN | — | NO | |
| readdir01 | NOT_RUN | — | NO | |
| readdir21 | NOT_RUN | — | NO | |

### dup / dup2 / dup3 (18 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| dup01-07 | TPASS ✅ | PASS | YES | all 7 pass |
| dup201-207 | TPASS ✅ | PASS | YES | all 7 pass |
| dup3_01 | TPASS ✅ | PASS | YES | O_CLOEXEC 已修复 |
| dup3_02 | NOT_RUN | — | NO | |

### fcntl 基础 (14+26 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| fcntl01-05 | TPASS ✅ | PASS | YES | F_DUPFD |
| fcntl08 | TPASS ✅ | PASS | YES | F_GETFD/F_SETFD |
| fcntl13-14 | TPASS ✅ | PASS | YES | F_GETFL/F_SETFL |
| fcntl06-07,09-12 | NOT_RUN | — | NO | 基础项 |
| fcntl15-40 | NOT_RUN | FIXABLE_LATER | NO | OFD锁/pipe buffer/seals |

### pipe / pipe2 (18 测例, Priority: 7)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| pipe01-15 | NOT_RUN | — | NO | 辅助family |
| pipe2_01-02, pipe2_04 | NOT_RUN | — | NO | |

### creat (9 测例, Priority: 7)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| creat01,03,05 | TPASS ✅ | PASS | YES | |
| creat04,06-09 | NOT_RUN | — | NO | |

---

## Round-1 Family (目录操作 + ext4 metadata)

### mkdir / mkdirat (7 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| mkdir02 | TFAIL (2) | FIXABLE_LATER | NO | SGID 继承 (Round-2 权限) |
| mkdir03 | NOT_RUN | — | NO | |
| mkdir04 | TFAIL (1) | FIXABLE_NOW | NO | 非存在中间目录→成功 (VFS bug) |
| mkdir05 | NOT_RUN | — | NO | |
| mkdir09 | NOT_RUN | — | NO | |
| mkdirat01-02 | NOT_RUN | — | NO | |

### rmdir (3 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| rmdir01 | TPASS (1/1) ✅ | PASS | YES | Round A: skip 移除 |

### unlink / unlinkat (5 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| unlink05 | TPASS (2/2) ✅ | PASS | YES | 文件+fifo |
| unlink07 | TPASS (6/6) ✅ | PASS | YES | ENAMETOOLONG 已修复 |
| unlink08 | TPASS (2/4) | — | YES | 2 EISDIR ✅, 2 EACCES (权限Round-2) |
| unlink09 | NOT_RUN | — | NO | |
| unlinkat01 | NOT_RUN | — | NO | |

### rename / renameat (16 测例, Priority: 10)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| rename01,03-14 | NOT_RUN | — | NO | 镜像中存在 |
| renameat01 | NOT_RUN | — | NO | |
| renameat201-202 | — | UNSUPPORTED | NO | RENAME_EXCHANGE/RENAME_NOREPLACE |

### truncate / ftruncate (7 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| ftruncate01 | TPASS ✅ | PASS | YES | |
| ftruncate03 | TPASS (4/4) ✅ | PASS | YES | Round A: EINVAL + B: EFBIG 修复 |
| ftruncate04 | NOT_RUN | — | NO | |
| truncate02 | NOT_RUN | — | NO | |
| truncate03 | TPASS (8/8) ✅ | PASS | YES | Round A: EACCES+R2: EFBIG 修复 |

### symlink / readlink (8 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| symlink01 | TPASS (5/5) ✅ | PASS | YES | Round A: ENAMETOOLONG 修复 |
| symlinkat01 | NOT_RUN | — | NO | |
| readlink01 | TPASS ✅ | PASS | YES | |
| readlink03 | TPASS (5/8) | — | NO | ENAMETOOLONG ✅, succeeded×2+ENOENT×1 |
| readlinkat01-02 | NOT_RUN | — | NO | |

### link / linkat (8 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| link02 | TPASS ✅ | PASS | YES | |
| link04 | TPASS (12/14) | — | YES | ENAMETOOLONG ✅, 空路径✅, EACCES×2 |
| link05 | TPASS ✅ | PASS | YES | 1000硬链接 |
| link08 | NOT_RUN | — | NO | |
| linkat01-02 | NOT_RUN | — | NO | |

### chmod / fchmod / fchmodat (13 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| chmod01,03,05-07 | NOT_RUN | — | NO | |
| fchmod01-06 | NOT_RUN | — | NO | |
| fchmodat01-02 | NOT_RUN | — | NO | |

### chown 系列 (15+ 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| chown01-05 (_16) | NOT_RUN | FIXABLE_LATER | NO | 需 seteuid/setreuid |
| fchown01-05 (_16) | NOT_RUN | FIXABLE_LATER | NO | |
| fchownat01-02 | NOT_RUN | FIXABLE_LATER | NO | |

### utime / utimes / utimensat (9 测例, Priority: 8)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| utime01-07 | NOT_RUN | — | NO | utime01-03 之前 TBROK (no free device) |
| utimes01 | NOT_RUN | — | NO | |
| utimensat01 | NOT_RUN | — | NO | |

### flock (5 测例, Priority: 6)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| flock01-04,06 | NOT_RUN | — | NO | |

### fallocate (6 测例, Priority: 6)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| fallocate01-06 | NOT_RUN | — | NO | |

---

## Round-2 Family (page cache / 一致性 / mmap)

### pread / pwrite (6+ 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| pread01-02 | NOT_RUN | — | NO | |
| pwrite01-04 | NOT_RUN | — | NO | |
| preadv01-03 | NOT_RUN | — | NO | |
| pwritev01-03 | NOT_RUN | — | NO | |
| preadv201-203 | — | UNSUPPORTED | NO | Linux 5.x+ |

### readv / writev (9+ 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| readv01-02 | NOT_RUN | — | NO | |
| writev01-07 | NOT_RUN | — | NO | |

### fsync / sync / syncfs (7 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| fsync01-04 | NOT_RUN | — | NO | |
| sync01 | NOT_RUN | — | NO | |
| syncfs01 | NOT_RUN | — | NO | |
| sync_file_range01-02 | NOT_RUN | — | NO | |

### mmap file-backed (21 测例, mmapstress excluded)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| mmap001,01-20 | NOT_RUN | — | NO | |
| mmapstress01-10 | — | DANGEROUS_STRESS | NO | |

### msync / munmap / mprotect / madvise (23 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| msync01-04 | NOT_RUN | — | NO | |
| munmap01-03 | NOT_RUN | — | NO | |
| mprotect01-05 | NOT_RUN | — | NO | |
| madvise01-11 | NOT_RUN | — | NO | |

### sendfile (8 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| sendfile02-09 | NOT_RUN | — | NO | |

---

## 辅助 Family

### statfs / fstatfs / statvfs (8 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| statfs01-03 | NOT_RUN | — | NO | |
| fstatfs01-02 | NOT_RUN | — | NO | |
| statvfs01-02 | NOT_RUN | — | NO | |

### umask / pathconf / fpathconf / realpath (5 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| umask01 | NOT_RUN | — | NO | |
| pathconf01-02 | NOT_RUN | — | NO | |
| fpathconf01 | NOT_RUN | — | NO | |
| realpath01 | NOT_RUN | — | NO | |

### readahead (2 测例)

| Testcase | 结果 | 分类 | 回归 | 备注 |
|----------|------|------|------|------|
| readahead01-02 | NOT_RUN | — | NO | |

---

## 回归集 (~50 TPASS)

| Family | 测例 |
|--------|------|
| open | open03, open04 |
| close | close01, close02 |
| read | read01, read02 |
| write | write01, write02 |
| lseek | lseek01 |
| stat | stat02, fstat02, lstat02 |
| access | access01 |
| getcwd | getcwd01 |
| chdir | chdir04 |
| getdents | getdents01 |
| dup | dup01-07, dup201-207, dup3_01 |
| fcntl | fcntl01-05, fcntl08, fcntl13-14 |
| creat | creat01, creat03, creat05 |
| unlink | unlink05(2/2), unlink07(6/6), unlink08(2/4) |
| link | link02, link04(12/14), link05 |
| readlink | readlink01, readlink03(5/8) |
| ftruncate | ftruncate01 |
| chown | chown01 |

---

## 强制排除清单 (~200+)

### Mount 系统状态（FS-Round-MNT）

> **详细计划**: `Doc/ltp_mount_plan.md`

| syscall | 当前能力 | 缺失 |
|---------|---------|------|
| `mount(40)` | 创建新 RamFS 挂载 | MS_BIND/MS_REC/MS_MOVE/MS_REMOUNT |
| `umount2(39)` | 基础卸载 + MNT_FORCE | MNT_DETACH 未完整实现 |

**基础设施状态**:
- MountFS / MountFSInode / mountpoints BTreeMap ✅
- add_mount / remove_mount / umount ✅
- 启动时静态挂载 (/dev, /proc, /tmp) ✅
- `/proc/mounts` ✅
- MS_BIND/MS_REC/MS_MOVE 常量定义 ✅
- bind mount 逻辑 ❌ (`filesystemtype=NULL → EINVAL`, 无 source 解析)
- 递归 bind / mount propagation / mount namespace ❌

**fs_bind 系列 (96 脚本)**:

| 子目录 | 数量 | 依赖 | Phase |
|--------|------|------|-------|
| bind/ | 25 | MS_BIND | MNT-1 |
| rbind/ | 40 | MS_BIND + MS_REC | MNT-2 |
| move/ | 22 | MS_MOVE | MNT-3 (可选) |
| cloneNS/ | 7 | CLONE_NEWNS | 后续专项 |
| 杂项 | 2 | — | MNT-1/MNT-2 |

**当前 fs_bind 状态**: 所有脚本在 `mount --bind` 第一步返回 EINVAL（`filesystemtype=NULL` 被第 2114 行拦截）。

### UNSUPPORTED — 不支持的特性

| 类别 | 测例数 | 代表 |
|------|--------|------|
| xattr | ~32 | setxattr*, getxattr*, listxattr* |
| ACL | ~1 | tacl_xattr.sh |
| quota | ~9 | quotactl01-09 |
| fanotify | ~25 | fanotify01-25 |
| inotify | ~14 | inotify01-12 |
| chroot/pivot_root | ~5 | chroot01-04 |
| landlock | ~10 | Linux 5.13+ |
| io_uring | ~3 | Linux 5.1+ |
| userfaultfd | ~6 | Linux 4.3+ |
| memfd_create | ~4 | Linux 3.17+ |
| statmount/listmount | ~13 | Linux 6.8+ |
| fsconfig/fsmount/fsopen | ~9 | Linux 5.2+ |
| openat2 | ~3 | Linux 5.6+ |
| close_range | ~2 | Linux 5.9+ |
| faccessat2 | ~2 | Linux 5.8+ |
| fchmodat2 | ~2 | Linux 6.3+ |
| renameat2 复杂flag | ~2 | RENAME_EXCHANGE/RENAME_NOREPLACE |
| statx | ~12 | Linux 4.11+ |

### DANGEROUS_STRESS — 压力/破坏性

| 测例 | 说明 |
|------|------|
| fsstress, fsx-linux, doio, iogen, growfiles | FS 压力 |
| ftest01-08, fs_racer(10), read_all, fs_fill | 极限测试 |
| fs_di, fsplough, stream01-05, lftest | 大文件/碎片 |
| openfile, inode01-02 | 资源耗尽 |
| mmapstress01-10 | mmap 压力 |

### FIXABLE_LATER — 依赖未来 Round

| 类别 | 依赖 |
|------|------|
| chown 全系列 (~15) | seteuid/setreuid |
| chmod 复杂权限 (~10) | 权限模型 |
| sticky bit (open01) | 权限模型 |
| S_ISGID 继承 (mkdir02) | 权限模型 |
| EACCES 权限校验 (unlink08/link04各2点) | 权限模型 |
| flock 完整语义 (~5) | Round-2 |
| fcntl OFD 锁 (fcntl15-40) | Round-2 |
| fallocate 复杂模式 | Round-2 |

---

## 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-05-25 | Round A+B: 修复 14 个 TFAIL→TPASS (O_APPEND, fchown, fcntl GETFL/SETFL, truncate EACCES+EFBIG+search, symlink ENAMETOOLONG+权限, rename/rmdir skip 移除) |
| 2026-05-22 | Mount 专项: fs_bind 从 UNSUPPORTED 移到 FS-Round-MNT, 新增 mount 系统状态表, 创建 `Doc/ltp_mount_plan.md` |
| 2026-05-22 | 重写: Round-0 全PASS, Round-1 部分PASS, 本地摸底360个二进制清单, 回归集~50 |
| 2026-05-20 | 创建文档, Oracle审查通过 |
