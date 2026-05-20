# FS-LTP Testcase 状态表

> 最后更新: 2026-05-20
> 当前阶段: Phase 0 — 体系建设中
> 当前 Round: FS-Preflight (尚未开始运行)

## 字段说明

| 字段 | 说明 |
|------|------|
| **Testcase** | LTP 测例名 |
| **Round** | 所属 FS Round (0/1/2/3) |
| **Family** | 所属 syscall family (如 open, read, stat) |
| **Arch** | rv64 / la64 / both |
| **Libc** | musl / glibc / both |
| **运行结果** | TPASS / TFAIL / TBROK / TCONF / PANIC / TIMEOUT / NOT_RUN |
| **行动分类** | PASS / FIXABLE_NOW / FIXABLE_LATER / UNSUPPORTED / ENV_FAIL / DANGEROUS_STRESS |
| **回归集** | YES / NO |
| **失败层次** | A-L (见 ltp_fs_plan.md §2.1) |
| **日志路径** | 运行日志位置 |
| **备注** | 失败原因 / 依赖说明 / 排除理由 |

---

## 三列表概览

| 列表 | 当前数量 | 说明 |
|------|----------|------|
| **回归集** | 0 (需从 Preflight 开始累积) | 历史已验证通过的测例，每次修复后全量回归 |
| **探索集** | ~100 (Round-0 核心 family) | 当前 round 要验证的测例 |
| **强制排除集** | ~500+ | 硬门禁排除 (UNSUPPORTED/DANGEROUS_STRESS/FIXABLE_LATER) |

### 重要提醒
- `os_test.conf` 中的 `ltp_include` 列表仅为人工精选的子集（~96个），**不代表测例已通过**
- 判断 PASS 必须有具体日志证据（arch + libc + run_id）
- `ltp_include` 中混有 `diotest*` / `crash*` 等已被本计划排除的测例，需要清理

---

## FS-Round-0 核心 Family 测例状态

### Family: open (18 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| open01 | 0 | rv64 | musl+glibc | TFAIL: sticky bit | FIXABLE_LATER | NO | F | batch2 | Round-1 权限模型 |
| open02 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open06 | 0 | — | — | NOT_RUN | — | NO | — | — | 注: 无 open05 |
| open07 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open08 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open09 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open10 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open11 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open12 | 0 | — | — | NOT_RUN | — | NO | — | — | 需 open12_child |
| open13 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open14 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| open15 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| openat01 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| openat02 | 0 | — | — | NOT_RUN | — | NO | — | — | 需 openat02_child |
| openat03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| openat04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: close (4 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| close01 | 0 | rv64 | musl+glibc | TPASS (3/3×2) | PASS | YES | — | preflight r1/r2/r3 | ✅ 3轮连续稳定 |
| close02 | 0 | — | — | NOT_RUN | — | NO | — | — | 待扩展 |
| close_range01 | — | — | — | — | UNSUPPORTED | NO | K | — | 需要 Linux 5.9+ |
| close_range02 | — | — | — | — | UNSUPPORTED | NO | K | — | 需要 Linux 5.9+ |

### Family: read (4 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| read01 | 0 | rv64 | musl+glibc | TPASS (1/1×2) | PASS | YES | — | batch2 | ✅ |
| read02 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| read03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| read04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: write (6 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| write01 | 0 | rv64 | musl | TPASS (1/1) | PASS | YES | — | batch2 | ✅ |
| write01 | 0 | rv64 | glibc | TFAIL: ENOSPC(28) | ENV_FAIL | NO | J | batch2 | glibc 镜像磁盘满 |
| write02 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| write03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| write04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| write05 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| write06 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: lseek (11 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| lseek01 | 0 | rv64 | musl+glibc | TPASS (4/4×2) | PASS | YES | — | batch2 | SEEK_SET/CUR/END ✅ |
| lseek02-10 | 0 | — | — | NOT_RUN | — | NO | — | — | 基础 SEEK_SET/CUR/END + 错误处理 |
| lseek11 | 0 | — | — | NOT_RUN | — | NO | — | — | SEEK_DATA/SEEK_HOLE，可能 UNSUPPORTED |

### Family: stat/fstat/lstat (9 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| stat01 | 0 | rv64 | musl+glibc | TBROK: getpwnam ENOENT | ENV_FAIL | NO | J | batch2 | /etc/passwd 缺 nobody |
| stat02-04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| fstat02-03 | 0 | — | — | NOT_RUN | — | NO | — | — | 注: 无 fstat01 |
| lstat01-03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| fstatat01 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: access/faccessat (9 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| access01-04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| faccessat01-02 | 0 | — | — | NOT_RUN | — | NO | — | — | 之前在 ltp_include 中有 faccessat01/02/201 |
| faccessat201-202 | — | — | — | — | UNSUPPORTED | NO | K | — | 需要 Linux 5.8+ |

### Family: getcwd/chdir (10 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| getcwd01-04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| chdir01-02, chdir04 | 0 | — | — | NOT_RUN | — | NO | — | — | chdir04 在 ltp_include 中 |
| fchdir01-03 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: getdents/readdir (4 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| getdents01-02 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| readdir01, readdir21 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### Family: dup/dup2/dup3 (16 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| dup01-07 | 0 | — | — | NOT_RUN | — | NO | — | — | 部分在 ltp_include 中 |
| dup201-207 | 0 | — | — | NOT_RUN | — | NO | — | — | 部分在 ltp_include 中 |
| dup3_01-02 | 0 | — | — | NOT_RUN | — | NO | — | — | dup3_01 在 ltp_include 中 |

### Family: fcntl 基础 (fcntl01-14, 14 测例)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| fcntl01-05 | 0 | — | — | NOT_RUN | — | NO | — | — | F_DUPFD |
| fcntl06-10 | 0 | — | — | NOT_RUN | — | NO | — | — | F_GETFD/F_SETFD |
| fcntl11-14 | 0 | — | — | NOT_RUN | — | NO | — | — | F_GETFL/F_SETFL |

### 辅助 Family: pipe/pipe2 (18 测例，不阻塞晋级)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| pipe01-15 | 0 | — | — | NOT_RUN | — | NO | — | — | — |
| pipe2_01-02, pipe2_04 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

### 辅助 Family: creat (9 测例，不阻塞晋级)

| Testcase | Round | Arch | Libc | 运行结果 | 行动分类 | 回归集 | 失败层次 | 日志 | 备注 |
|----------|-------|------|------|----------|----------|--------|----------|------|------|
| creat01,03,05 | 0 | — | — | (待验证) | — | NO | — | — | 在 ltp_include 中，需验证 |
| creat04,06-09 | 0 | — | — | NOT_RUN | — | NO | — | — | — |

---

## 强制排除清单（不进入任何 Round）

### UNSUPPORTED — MangoCore 不支持的特性

| 类别 | 测例数(约) | 代表测例 | 原因 |
|------|-----------|----------|------|
| xattr 系列 | ~32 | setxattr*, getxattr*, fsetxattr*, listxattr*, removexattr* | 扩展属性未实现 |
| ACL | ~1 | tacl_xattr.sh | 访问控制列表未实现 |
| quota | ~9 | quotactl01-09 | 磁盘配额未实现 |
| namespace/bind | ~7 | fs_bind_cloneNS* | 命名空间隔离未实现 |
| mount propagation | ~85 | fs_bind* (bind/rbind/move 共 85 个脚本) | 挂载传播未实现 |
| fanotify | ~25 | fanotify01-25 | 文件通知框架未实现 |
| inotify | ~14 | inotify01-12, inotify_init* | inode 通知框架未实现 |
| chroot/pivot_root | ~5 | chroot01-04, pivot_root01 | 根目录切换未实现 |
| landlock | ~10 | landlock01-10 | Linux 5.13+ 安全模块 |
| io_uring | ~3 | io_uring01-03 | Linux 5.1+ 异步 I/O |
| userfaultfd | ~6 | userfaultfd01-06 | Linux 4.3+ |
| memfd_create | ~4 | memfd_create01-04 | Linux 3.17+ |
| statmount/listmount | ~13 | statmount01-09, listmount01-04 | Linux 6.8+ |
| fsconfig/fsmount/fsopen | ~9 | fsconfig01-03, fsmount01-02, fsopen01-02, fspick01-02, move_mount01-03 | Linux 5.2+ mount API |
| openat2 | ~3 | openat201-203 | Linux 5.6+ |
| close_range | ~2 | close_range01-02 | Linux 5.9+ |
| faccessat2 | ~2 | faccessat201-202 | Linux 5.8+ |
| fchmodat2 | ~2 | fchmodat2_01-02 | Linux 6.3+ |
| file_attr (chattr/lsattr) | ~5 | file_attr01-05 | 需要 ext2/3/4 特殊 ioctl |
| renameat2 | 见 FIXABLE_LATER | renameat201-202 | 仅复杂 flag 排除 |
| direct I/O | ~6 | diotest* | O_DIRECT 不支持 |
| swap | ~5 | swapon*, swapoff* | 需要 CAP_SYS_ADMIN + swap 支持 |
| mknod (设备节点) | ~11 | mknod01-09, mknodat01-02 | 设备节点创建不支持 |
| name_to_handle_at | ~3 | name_to_handle_at01-03 | 需要 CAP_DAC_READ_SEARCH |
| open_by_handle_at | ~2 | open_by_handle_at01-02 | 需要 CAP_DAC_READ_SEARCH |

### DANGEROUS_STRESS — 压力/破坏性测试

| 测例 | 原因 | 后期处置 |
|------|------|----------|
| fsstress | FS 随机操作压力测试 | Round-3 单独隔离运行 |
| fsx-linux | FS 一致性压力测试 | Round-3 |
| doio | I/O 压力工具 | Round-3 |
| iogen | I/O 生成器 | Round-3 |
| growfiles | 文件增长压力 | Round-3 |
| ftest01-08 | 文件系统功能压力套件 | Round-3 |
| fs_racer (10 scripts) | FS 并发竞争测试 | Round-3 |
| read_all | 全文件系统读取压力 | Round-3 |
| fs_fill | 填满 FS 测试 | Round-3 |
| fs_di (frag) | FS 碎片测试 | Round-3 |
| fsplough | 目录树遍历压力 | Round-3 |
| stream01-05 | 大文件流式 I/O 压力 | Round-3 |
| lftest | >2GB 大文件测试 | Round-3 |
| openfile | 最大打开文件数测试 | Round-3 |
| inode01-02 | inode 耗尽测试 | Round-3 |

### FIXABLE_LATER — 依赖后续 Round

| 测例家族 | Round | 依赖 |
|----------|-------|------|
| chmod/fchmod/fchmodat (~15) | 1 | Round-0 基础读写 + 权限模型 |
| chown/fchown/fchownat (~15) | 1 | Round-0 基础读写 + uid/gid |
| truncate/ftruncate (~5) | 1 | Round-0 read/write + ext4 metadata |
| mkdir/mkdirat (~7) | 1 | Round-0 目录读取 |
| rmdir (~3) | 1 | Round-1 mkdir |
| unlink/unlinkat (~6) | 1 | Round-0 基础 fd/inode 生命周期 |
| rename/renameat (~16) | 1 | Round-0 path 解析 + Round-1 unlink |
| link/linkat (~6) | 1 | Round-0 inode 生命周期 |
| symlink/symlinkat (~4) | 1 | Round-0 path 解析 |
| readlink/readlinkat (~4) | 1 | Round-1 symlink |
| utime/utimes/utimensat (~10) | 1 | Round-0 stat 基础 |
| flock (~6) | 1 | Round-0 fd table + 锁语义 |
| fcntl15-40 (~26) | 1 | Round-0 fcntl 基础 |
| renameat2 (复杂 flag) | 1 | Round-1 rename 基础 |
| fallocate (~6) | 2 | Round-0/1 基础读写 + ext4 metadata |
| fsync/fdatasync (~7) | 2 | Round-0/1 + page cache 回写 |
| sync/syncfs (~2) | 2 | Round-2 fsync + 全局同步 |
| mmap file-backed (~12) | 2 | Round-0/1 + page cache 一致性 |
| msync (~4) | 2 | Round-2 mmap |
| pread/pwrite 完整语义 (~6) | 2 | Round-0 read/write offset smoke |
| readv/writev (~8) | 2 | Round-0 read/write |
| sendfile (~8) | 3 | Round-2 page cache 一致性 |
| splice (~9) | 3 | Round-2 pipe + page cache |
| copy_file_range (~3) | 3 | Round-2 page cache |
| statx (~12) | 2 | Round-0 stat 基础 |

### 非 FS Round 范围（不处理）

| 测例家族 | 所属范围 | 说明 |
|----------|----------|------|
| socket/bind/connect/accept/send/recv 等 | 网络 | ~92 测例 |
| signal/sigaction/sigprocmask 等 | 信号 | ~21 测例 |
| clone/fork/exec/wait 等 | 进程 | — |
| futex | 同步 | — |
| epoll/poll/select（网络 fd 相关） | 网络/poll | 与 FS 共用 fd table，仅基础 fd 操作有交集 |

---

## Preflight 进度

### FS-Preflight 状态: SMOKE_PASSED

| 检查项 | 状态 | 备注 |
|--------|------|------|
| LTP inline runner 连续 3 轮无 panic | ✅ PASS | close01/creat01/dup01 3轮 musl+glibc 全 PASS |
| timeout case 正确 skip，后续不受影响 | ✅ PASS | 非 include 测例正确跳过，exit=0 |
| ltp_include/exclude/from 配置机制生效 | ✅ PASS | include=["close01","creat01","dup01"] 过滤正确 |
| 镜像恢复机制正常 | ✅ PASS | 每轮 xz -dkc 恢复 + conf-inject 正常 |
| 扩展 batch 8 测例 | ✅ DONE | lseek01/read01 PASS, open01/stat01 ENV_FAIL/FIXABLE_LATER, write01 glibc ENV_FAIL |
| P0 panic 扫描 | 🔄 IN_PROGRESS | explore agents 扫描 FS+VM panic 点 |
| la64 编译验证 | 🔄 PENDING | — |

---

## 变更记录

| 日期 | 变更内容 |
|------|----------|
| 2026-05-20 | 创建文档。Oracle 审查后重写：加入运行结果/行动分类分离、arch/libc 维度、三列表、LTP 上游完整清单、Preflight 阶段 |
