# LTP Filesystem Mega-Fix: 16-Stage Conservative Repair

## TL;DR

> **Quick Summary**: 分阶段修复 MangoCore 内核中 LTP filesystem 相关 TFAIL/TBROK，从基础 VFS 路径解析到高级 syscall 语义，按优先级逐步推进。保守策略：修复当前失败 + 按优先级启用 NOT_RUN，Stage 12-15 延期。

> **Deliverables**:
> - 当前 ~50 TPASS 回归集无回退
> - Stages 0-7: 核心 VFS 正确性修复（chdir/open/mkdir/rmdir/link/rename/stat/chmod/chown/umask）
> - Stages 8-11: FS syscall 扩展（fsync/fallocate, xattr user.*, procfs/devfs/sysfs 环境补齐, mount/chroot）
> - Stages 12-15: 延期（flock/ioctl/splice/quota/fanotify）

> **Estimated Effort**: XL (16 stages, ~50-80 tasks)
> **Parallel Execution**: Limited — most stages are sequential (each builds on previous)
> **Critical Path**: Stage 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 (core VFS chain)

---

## Context

### Original Request

用户要求在 MangoCore（`#![no_std]` Rust 内核，riscv64 + loongarch64）中修复 LTP fs 相关失败。16 个阶段从基础到高级，每个阶段由 Oracle 审查通过后放行。优先补齐接近 Linux 语义的通用 VFS/FS 行为，参考 DragonOS、Linux man page、LTP 源码。

### Interview Summary

**Key Discussions**:
- **Scope**: 保守修复 — 优先修 TFAIL/TBROK，NOT_RUN 按优先级启用，Stage 12-15 延期
- **statx/xattr**: 最小可用子集 — statx 修基础字段返回，xattr 只做 user.* namespace
- **Test platform**: rv64+glibc 每阶段验收，la64 每个 milestone 边界全量回归
- **DAC model**: 基础四件套（uid/gid/mode + umask + sticky + chmod/chown 权限检查），不做 capabilities
- **策略**: 禁止硬编码测试行为，必须实现可复用 Linux 语义；每个阶段累计重跑前序通过 case

**Research Findings**:
- VFS 层架构完整：IndexNode trait (~40 methods), File struct, FileSystem trait, MountFS/MountFSInode
- 关键代码缺口：umask 是 NO-OP、chmod/chown 无权限检查、sticky bit 不执行、ctime 多处不更新、xattr 未注册、statx mask 被忽略、flock 最简实现
- procfs 缺 /proc/cmdline, /proc/self/mountinfo, /proc/self/io, /proc/self/pagemap
- devfs 缺 /dev/full, /dev/loop*
- sysfs 仅 /sys/class/net
- Mount: bind/remount/move/propagation 代码完整但 LTP 语义未验证
- 路径解析: vfs_lookup 完整支持 symlink follow (max 40), trailing slash, AT_FDCWD

### Metis Review

**Identified Gaps** (addressed):
- 目标模糊 → 确认保守修复策略
- NOT_RUN vs FAIL 混淆 → 每个 case 标注 CURRENT_FAIL / NOT_RUN_ENABLE / DEFERRED
- Stage 1/10、Stage 3/11 重叠 → 重组为 milestone 结构
- xattr/statx 文档冲突 → 确认最小可用子集
- 16-stage 过大 → Stage 12-15 标记为 DEFERRED_STRETCH

### Momus Review
**Final Verdict**: OKAY (2026-06-10) — 计划可执行，核心引用已验证，QA 场景具体可操作。

---

## Work Objectives

### Core Objective

系统性修复 MangoCore VFS/FS 层与 Linux/LTP 语义差异，使核心 filesystem LTP 测试（chdir/open/mkdir/rmdir/link/rename/stat/chmod/chown/umask）在 rv64+glibc 上达到 TPASS，无回归。

### Concrete Deliverables

- `.sisyphus/plans/ltp-fs-megafix.md` — 本计划
- 每个 Stage 的代码修改（os/src/fs/, os/src/syscall/, os/src/task/ 等）
- 每个 Stage 的 LTP 验收记录
- 更新 `Doc/Work_Log.md` 和 `Doc/ltp/ltp_fs_status.md`

### Definition of Done

- [ ] Stages 0-7 全部 Oracle 审查通过
- [ ] 回归集 ~50 TPASS 无回退
- [ ] 每个阶段目标 LTP case 达到 TPASS（TCONF 允许，TFAIL/TBROK 不允许）
- [ ] 双架构 kernel build 通过（rv64 + la64）
- [ ] 无 kernel panic

### Must Have

> These are **stage-level deliverable gates** verified by Oracle at each stage boundary, not individual implementation tasks.

- [ ] Stage 0: 基线建立 — 当前 LTP 状态、单测复现方式 (详见 tasks 0.1-0.3)
- [ ] Stage 1: rootfs 环境修复 — /etc/passwd, /etc/group, /tmp 1777, /dev/full (详见 tasks 1.1-1.4)
- [ ] Stage 2: 路径解析 + 文件类型 errno 修复 — ENOTDIR, EACCES, ENOENT, EBADF, ELOOP (详见 tasks 2.1-2.8)
- [ ] Stage 3: 权限模型 — umask, chmod/chown 检查, sticky bit 执行 (详见 tasks 3.1-3.6)
- [ ] Stage 4: open/mkdir/rmdir/mknod flags 和 errno (详见 tasks 4.1-4.5)
- [ ] Stage 5: link/symlink/readlink 语义修复 (详见 tasks 5.1-5.3)
- [ ] Stage 6: rename/renameat2 类型检查和非空目录保护 (详见 tasks 6.1-6.3)
- [ ] Stage 7: stat/statx/statfs metadata 正确性、ctime 更新 (详见 tasks 7.1-7.4)

### Must NOT Have (Guardrails)

- 不允许硬编码 LTP 文件名/路径/进程名做特殊判断
- 不允许修改 LTP 测试本身
- 不允许伪造成功返回值
- 不允许为了单个 case 绕过 VFS 正常路径
- 不允许破坏已有 ~50 TPASS 回归集
- 不允许引入完整 capabilities/ACL/SELinux/namespace 子系统
- 不允许并行编译 rv64 和 la64

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed.

### Test Decision

- **Infrastructure exists**: YES (LTP suite runner, kernel-dev tools)
- **Automated tests**: Tests-after (LTP 作为验收)
- **Framework**: LTP syscalls suite via `kernel-dev_kernel_test_config` + `kernel-dev_kernel_run`
- **Agent QA**: 每个 stage 用 kernel-dev tools 运行目标 LTP case

### QA Policy

每个 stage 验收通过以下工具执行：
- `kernel-dev_kernel_build(arch="rv64")` — 编译验证
- `kernel-dev_kernel_build(arch="la64")` — 双架构编译验证（milestone 边界）
- `kernel-dev_kernel_test_config(arch="rv64", ...)` — 配置 LTP 测试参数
- `kernel-dev_kernel_run(arch="rv64", ...)` — 运行 LTP 并检查输出

期望输出：
- 每个目标 case `TPASS`
- 无 `TFAIL`
- 无 `TBROK`
- 无 `Kernel panic`
- 无 `panicked at`

---

## Execution Strategy

### Milestone Structure

```
Milestone A: Baseline + Environment (Stage 0 → 1)
  └─ Gate: Oracle review each stage; Stage 1 must have stable rootfs

Milestone B: Core VFS Correctness (Stage 2 → 3 → 4 → 5 → 6 → 7)
  └─ Gate: Oracle review each stage; stage N must pass all previous regression cases

Milestone C: FS Syscall Expansion (Stage 8 → 9 → 10 → 11)
  └─ Gate: Oracle review each stage; la64 regression at milestone boundary

Milestone D: Deferred/Stretch (Stage 12 → 13 → 14 → 15)
  └─ Gate: OPTIONAL — only if time permits
```

### Per-Stage Workflow

```
1. Oracle review of previous stage → GO received
2. Read LTP source for target case(s) — understand expected Linux semantics
3. Read MangoCore current code path — identify gap
4. Implement minimal fix (no hacks, no per-case special handling)
5. rv64 kernel build → PASS
6. Run target LTP case(s) + full regression set → verify
7. If regression → fix immediately before continuing
8. Update Doc/Work_Log.md
9. Submit stage for Oracle review
```

### Agent Dispatch Summary

由于 stages 之间高度依赖（每个 stage 基于前一个 stage 的代码修改），并行度有限。每个 stage 内部的任务可以在一定程度上并行。

- **Stage 0**: 2-3 tasks (log analysis, doc update, reproduce setup) — parallel
- **Stage 1**: 3-5 tasks (rootfs files, dir permissions, env setup) — parallel
- **Stage 2**: 3-5 tasks (path errno, file type checks, symlink follow) — mostly parallel
- **Stage 3**: 4-6 tasks (umask, chmod check, chown check, sticky bit, ctime) — parallel
- **Stage 4**: 3-5 tasks (open flags, mkdir errno, rmdir errno, mknod) — parallel
- **Stage 5**: 3-4 tasks (link errno, symlink errno, readlink, link count) — parallel
- **Stage 6**: 3-4 tasks (rename types, renameat2 flags, overwrite checks) — parallel
- **Stage 7**: 4-6 tasks (statx fix, statfs, ctime updates, timestamps) — parallel
- **Stage 8**: 3-4 tasks (fsync, fallocate, lseek, copy_file_range)
- **Stage 9**: 3-4 tasks (xattr storage, syscall reg, set/get/list/remove)
- **Stage 10**: 3-5 tasks (procfs gaps, devfs gaps, sysfs gaps)
- **Stage 11**: 3-4 tasks (mount errno, chroot, readonly enforcement)
- **Stage 12-15**: DEFERRED

---

## TODOs

### Milestone A: Baseline + Environment

---

#### Stage 0: 建立基线和复现方式

- [ ] 0.1 收集当前 LTP FS 基线数据

  **What to do**: 运行 kernel-dev_kernel_status 确认环境；配置 kernel-dev_kernel_test_config 单测 chdir01/chmod05/umask01/open11/linkat01/rename04/statx03/setxattr01/mount02/fsync04；运行 kernel-dev_kernel_run 解析 TPASS/TFAIL/TBROK/TCONF。记录 errno 差异和 setup 失败原因。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 0 (with 0.2, 0.3), Blocked by None
  **References**: Doc/ltp/ltp_fs_status.md, Doc/ltp/ltp_workflow.md, os_test.conf, user/src/bin/initproc.rs
  **Acceptance Criteria**: 基线数据文件存在 .sisyphus/evidence/stage0-baseline.md。每个验收 case 有明确状态和 errno 差异记录。
  **QA**: kernel-dev_kernel_run with ltp_include=chdir01,chmod05,umask01,open11, expect TPASS/TFAIL/TBROK/TCONF per case. Evidence: stage0-baseline.txt

- [ ] 0.2 解析 qemu.log 中所有 FS 相关失败

  **What to do**: 查找最近 LTP 全量运行日志。grep fs 相关 TFAIL/TBROK/TCONF。按 family 分组统计。标注 CURRENT_FAIL/ENV_FAIL/DEFERRED。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 0 (with 0.1, 0.3), Blocked by None
  **References**: Doc/ltp/ltp_fs_status.md:22-28, Doc/ltp/ltp_fs_plan.md:24-45
  **Acceptance Criteria**: FS 失败统计输出到 .sisyphus/evidence/stage0-fs-failures.md
  **QA**: Bash grep on latest LTP log, count per family. Evidence: stage0-fs-failures.txt

- [ ] 0.3 更新 LTP FS 状态文档

  **What to do**: 基于 0.1 和 0.2 更新 Doc/ltp/ltp_fs_status.md。新增 CURRENT_FAIL/NOT_RUN_ENABLE/DEFERRED 分类。记录回归集完整 case 列表。

  **Recommended Agent Profile**: Category writing, Skills ["mango-worklog"]
  **Parallelization**: Wave 0 (with 0.1, 0.2), depends on 0.1 + 0.2 data
  **References**: Doc/ltp/ltp_fs_status.md, Doc/ltp/ltp_fs_plan.md:343-363
  **Acceptance Criteria**: 日期戳更新。回归集列表完整。新增三分类。变更记录追加。
  **QA**: grep CURRENT_FAIL\|NOT_RUN_ENABLE\|DEFERRED in Doc/ltp/ltp_fs_status.md. Evidence: stage0-doc-update.txt

---

#### Stage 1: rootfs / 测试环境 / 基础目录结构小修

- [ ] 1.1 确保 /etc/passwd 和 /etc/group 存在且内容正确

  **What to do**: 检查 ensure_ltp_compat_etc_files()。验证 passwd 含 root(0) + nobody(65534)。验证 group 含 root(0) + nogroup(65534) + nobody(65534) + daemon(1)。缺失时补齐。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 1 (with 1.2, 1.3, 1.4), Blocked by Stage 0
  **References**: os/src/fs/mod.rs, os/src/main.rs rust_main(), linux man 5 passwd/group
  **Acceptance Criteria**: /etc/passwd 和 /etc/group 存在且内容正确。chmod07/fchmod02 setup 不再 TBROK。rv64 build PASS。
  **QA**: kernel-dev_kernel_run with ltp_include=chmod07,fchmod02, expect setup no TBROK. Evidence: stage1-etc.txt

- [ ] 1.2 修复 /tmp 权限为 1777 (sticky bit)

  **What to do**: 检查 /tmp 挂载代码 mode 参数。确保为 InodeMode::S_IRWXUGO | S_ISVTX。同修 /dev/shm。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 1, Blocked by Stage 0
  **References**: os/src/fs/mod.rs:150-160, os/src/fs/vfs/mod.rs InodeMode::S_ISVTX
  **Acceptance Criteria**: /tmp mode=01777, /dev/shm mode=01777。rv64 build PASS。
  **QA**: kernel-dev_kernel_run with stat /tmp, verify S_ISVTX. Evidence: stage1-tmp.txt

- [ ] 1.3 补齐缺失的基础目录: /mnt, /run, /var/tmp

  **What to do**: 在 rootfs init 中创建 /mnt(0755), /run(0755), /var/tmp(01777)。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 1, Blocked by Stage 0
  **References**: os/src/fs/mod.rs mount_common_filesystems(), os/src/fs/vfs/index_node.rs:217 mkdir()
  **Acceptance Criteria**: 三个目录存在且 mode 正确。rv64 build PASS。
  **QA**: kernel-dev_kernel_run stat /mnt /run /var/tmp. Evidence: stage1-dirs.txt

- [ ] 1.4 确保 /dev, /proc, /sys 稳定存在 + 添加 /dev/full

  **What to do**: 验证 mount_common_filesystems 稳定性。添加 /dev/full (major 1 minor 7): read 返回零, write 返回 ENOSPC。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 1, Blocked by Stage 0
  **References**: os/src/fs/mod.rs:119-200, os/src/fs/dev/null.rs, os/src/fs/dev/zero.rs
  **Acceptance Criteria**: /dev/full write->ENOSPC(28), read->0 bytes。rv64 build PASS。
  **QA**: kernel-dev_kernel_run with inline test write to /dev/full, expect ENOSPC. Evidence: stage1-full.txt

---

### Milestone B: Core VFS Correctness

---

#### Stage 2: 路径解析、文件类型检查、基础 errno

- [ ] 2.1 修复 chdir 对非目录返回 ENOTDIR

  **What to do**: 跟踪 sys_chdir 实现，vfs_lookup 返回后验证目标 FileType==Dir，否则返回 ENOTDIR(20)。同修 sys_fchdir。

  **Must NOT do**: 不提前在 vfs_lookup 中检查；不对 symlink-to-dir 返回 ENOTDIR。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2a (with 2.2, 2.3), Blocked by Stage 1

  **References**: os/src/syscall/fs.rs sys_chdir/sys_fchdir, os/src/fs/mod.rs:501 vfs_lookup, linux man 2 chdir

  **Acceptance Criteria**: chdir to regular file -> ENOTDIR; chdir to symlink-to-dir -> success. LTP chdir01 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=chdir01, expect TPASS. Evidence: stage2-chdir01.txt

- [ ] 2.2 修复路径中间组件非目录时返回 ENOTDIR

  **What to do**: vfs_lookup 组件遍历中检查每个中间组件 FileType==Dir，不是则返回 ENOTDIR。Symlink-to-dir 先 follow 再检查。

  **Must NOT do**: 不对最后一个组件做此检查。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2a, Blocked by Stage 1

  **References**: os/src/fs/mod.rs:501-610 vfs_lookup loop, linux man 2 open ENOTDIR

  **Acceptance Criteria**: open("/file/in/path") where "file" is regular -> ENOTDIR. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=pathconf02, expect TPASS. Evidence: stage2-pathenotdir.txt

- [ ] 2.3 修复不存在路径返回 ENOENT (优先级正确)

  **What to do**: 确保 vfs_lookup 中 ENOENT 在 ENOTDIR 之后检查。不存在的路径组件应返回 ENOENT。

  **Must NOT do**: 不改变现有 ENOENT 语义（很多地方已正确处理）。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2a, Blocked by Stage 1

  **References**: os/src/fs/mod.rs:501 vfs_lookup, linux man 2 open

  **Acceptance Criteria**: open nonexistent path -> ENOENT(-2). rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=open11, expect correct errno. Evidence: stage2-enoent.txt

- [ ] 2.4 修复 bad dirfd 返回 EBADF

  **What to do**: resolve_start_inode 中 fd_table.get_file(fd) 返回 err 时确保返回 EBADF(-9)。

  **Must NOT do**: 不修改 AT_FDCWD 处理。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2b (with 2.5, 2.6), Blocked by Stage 1

  **References**: os/src/syscall/fs.rs:104-113 resolve_start_inode

  **Acceptance Criteria**: openat(bad_fd, path) -> EBADF. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=openat04, expect EBADF. Evidence: stage2-badfd.txt

- [ ] 2.5 修复 trailing slash 指向非目录返回 ENOTDIR

  **What to do**: vfs_lookup 结束后检查 path 是否以 / 结尾，若是且目标为 non-dir -> ENOTDIR。

  **Must NOT do**: 不在 parse_path 中修改 trailing slash 处理（当前静默丢弃）。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2b, Blocked by Stage 1

  **References**: os/src/fs/mod.rs:473 parse_path, linux man 2 open ENOTDIR

  **Acceptance Criteria**: open("regular_file/") -> ENOTDIR. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with inline test, expect ENOTDIR. Evidence: stage2-trailingslash.txt

- [ ] 2.6 lstat 不跟随最终 symlink; stat 跟随

  **What to do**: 确认 fstatat 中 AT_SYMLINK_NOFOLLOW flag 正确传递给 vfs_lookup 的 follow_final 参数。lstat=NOFOLLOW, stat=FOLLOW。

  **Must NOT do**: 不修改已有 symlink 逻辑。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2b, Blocked by Stage 1

  **References**: os/src/syscall/fs.rs sys_fstatat, os/src/fs/mod.rs:501 vfs_lookup follow_final

  **Acceptance Criteria**: lstat(symlink) -> symlink metadata; stat(symlink) -> target metadata. LTP lstat02 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=lstat02, expect TPASS. Evidence: stage2-lstat.txt

- [ ] 2.7 readlink/readlinkat 对非 symlink 返回 EINVAL

  **What to do**: sys_readlinkat 中先获取 metadata，检查 file_type==SymLink，不是则返回 EINVAL(-22)。

  **Must NOT do**: 不在 read_at 中做类型检查。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2c (with 2.8), Blocked by Stage 1

  **References**: os/src/syscall/fs.rs sys_readlinkat, linux man 2 readlink

  **Acceptance Criteria**: readlink on regular file -> EINVAL. LTP readlink03 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=readlink03, expect TPASS. Evidence: stage2-readlink.txt

- [ ] 2.8 readlinkat dirfd + AT_FDCWD 路径解析

  **What to do**: 确保 sys_readlinkat 正确使用 resolve_start_inode(dirfd) 并调用 vfs_lookup_parent_for_start。

  **Must NOT do**: 不重写 readlinkat 核心逻辑。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 2c, Blocked by Stage 1

  **References**: os/src/syscall/fs.rs sys_readlinkat

  **Acceptance Criteria**: readlinkat(AT_FDCWD, ...) 等价于 readlink(...). LTP readlinkat01 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=readlinkat01, expect TPASS. Evidence: stage2-readlinkat.txt

---

#### Stage 3: 权限、owner/group、mode、umask、chroot

- [ ] 3.1 实现 umask 系统调用

  **What to do**:
  - 在 TaskControlBlockInner (os/src/task/task.rs) 中添加 umask: u32 字段（当前 uid/gid 状态所在结构体，行 193-212）
  - sys_umask(mask) 返回旧 mask 并设置新 mask
  - 在 open_file_at 的 create 路径中: mode = mode & ~umask
  - 在 mkdirat 中同样应用 umask
  - fork/clone 时在 TaskControlBlock::sys_clone 中继承父进程 umask

  **Must NOT do**: 不修改 Stat::new() 的默认值；不在 fs 层做 umask。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 3a (with 3.2, 3.3), Blocked by Stage 1+2
  **References**: os/src/syscall/fs.rs:4453 sys_umask (current NO-OP), os/src/task/task.rs TaskControlBlockInner (uid/gid fields), os/src/syscall/fs.rs:269 open_file_at, linux man 2 umask
  **Acceptance Criteria**: umask(022) returns old mask; open(O_CREAT, 0666) creates file with mode 0644. LTP umask01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=umask01, expect TPASS. Evidence: stage3-umask.txt

- [ ] 3.2 chmod/fchmod/fchmodat 添加权限检查

  **What to do**:
  - sys_fchmodat 中检查: caller.uid==inode.uid OR caller.uid==0
  - 不是 owner 且不是 root -> EPERM(-1)
  - sys_fchmod 同样添加检查

  **Must NOT do**: 不做 capabilities 检查 (CAP_FOWNER)。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 3a, Blocked by Stage 1+2

  **References**: os/src/syscall/fs.rs:2172 sys_fchmodat, os/src/syscall/fs.rs:2213 sys_fchmod, os/src/syscall/fs.rs:173 open_subject_ids

  **Acceptance Criteria**: non-owner chmod -> EPERM; owner chmod -> success; root chmod -> success. LTP chmod05,chmod06 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=chmod05,chmod06, expect TPASS. Evidence: stage3-chmod.txt

- [ ] 3.3 chown/fchown/lchown 添加权限检查

  **What to do**:
  - sys_fchownat 中检查: caller.uid==0 (root)
  - 非 root -> EPERM(-1)
  - sys_fchown 同样添加
  - lchown (AT_SYMLINK_NOFOLLOW) 作用于 symlink 本身

  **Must NOT do**: 不做 CAP_CHOWN 检查；不实现非 root 改 group 的逻辑。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 3a, Blocked by Stage 1+2

  **References**: os/src/syscall/fs.rs:2246 sys_fchown, os/src/syscall/fs.rs:2282 sys_fchownat, linux man 2 chown

  **Acceptance Criteria**: non-root chown -> EPERM; root chown -> success. lchown on symlink affects symlink not target. LTP chown04,fchown04,lchown03 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=chown04,fchown04,lchown03, expect TPASS. Evidence: stage3-chown.txt

- [ ] 3.4 实现 sticky bit 执行 (unlink/rename 检查)

  **What to do**:
  - 在 sys_unlinkat 和 sys_renameat2 中: 若 parent dir 有 S_ISVTX，检查 caller.uid==file.uid OR caller.uid==parent.uid OR caller.uid==0
  - 不满足 -> EACCES(-13) 或 EPERM(-1)

  **Must NOT do**: 不对 root 做 sticky 检查。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 3b (with 3.5), Blocked by Stage 1+2

  **References**: os/src/syscall/fs.rs:2756 sys_unlinkat, os/src/syscall/fs.rs:2574 sys_renameat2, os/src/fs/vfs/mod.rs InodeMode::S_ISVTX, linux man 2 unlink

  **Acceptance Criteria**: non-owner unlink in sticky /tmp -> EACCES; owner unlink -> success. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=open01A, expect correct sticky behavior. Evidence: stage3-sticky.txt

- [ ] 3.5 创建文件时 mode 受 umask 影响

  **What to do**:
  - open_file_at 的 create 路径和 mkdirat 中应用 umask
  - mode = caller_mode & ~task.umask
  - 保持 S_IFMT 位不变

  **Must NOT do**: 不重复实现 3.1 的 umask。

  **Recommended Agent Profile**: Category quick, Skills []

  **Parallelization**: Wave 3b, Blocked by Stage 1+2

  **References**: os/src/syscall/fs.rs:269 open_file_at, os/src/syscall/fs.rs:2689 mkdirat, Depends on Task 3.1

  **Acceptance Criteria**: open(O_CREAT, 0666) with umask=022 -> file mode 0644. mkdir with umask -> dir mode correctly masked. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=umask01, verify mode values. Evidence: stage3-umask-create.txt

- [ ] 3.6 chroot 权限和类型检查（需注册 syscall + 实现基本根目录切换）

  **What to do**:
  - chroot syscall 当前不存在（无 sys_chroot, 无 SYSCALL_CHROOT）
  - 在 os/src/syscall/syscall_id.rs 添加 SYSCALL_CHROOT
  - 在 os/src/syscall/mod.rs dispatch 中注册
  - 实现 sys_chroot: vfs_lookup 解析目标路径 → 验证 FileType==Dir (ENOTDIR) → 验证 caller uid==0 (EPERM) → 更新进程 fs.working_inode 指向新根
  - 这是一个 **real chroot**: 进程后续路径解析将以新根为起点。路径 ".." 在根目录不穿越

  **Must NOT do**: 不做完整 mount namespace 隔离（此属于 Stage 11）；不假成功。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 3b (with 3.4, 3.5), Blocked by Stage 2
  **References**: os/src/syscall/mod.rs dispatch, os/src/syscall/syscall_id.rs, os/src/fs/mod.rs:501 vfs_lookup, os/src/task/task.rs FsStatus.working_inode, linux man 2 chroot — "chroot() changes the root directory of the calling process"
  **Acceptance Criteria**: chroot to file -> ENOTDIR; non-root -> EPERM; root to valid dir -> success; subsequent open("/") opens the new root. LTP chroot01,chroot02 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=chroot01,chroot02, expect TPASS. Evidence: stage3-chroot.txt

---

#### Stage 4: open/openat、mkdir/rmdir、mknod

- [ ] 4.1 O_CREAT|O_EXCL 已存在时返回 EEXIST

  **What to do**: open_file_at 中 vfs_lookup 成功后，若 flags 含 O_CREAT|O_EXCL，返回 EEXIST(-17)。当前代码 fs.rs:297 已有检查，验证正确。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 4a, Blocked by Stage 3
  **References**: os/src/syscall/fs.rs:297 O_CREAT|O_EXCL check
  **Acceptance Criteria**: open(O_CREAT|O_EXCL, existing_file) -> EEXIST. LTP open06 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=open06, expect TPASS. Evidence: stage4-eexist.txt

- [ ] 4.2 O_DIRECTORY 打开非目录返回 ENOTDIR

  **What to do**: open_file_at 中 vfs_lookup 成功后，若 flags 含 O_DIRECTORY 且 FileType!=Dir，返回 ENOTDIR(-20)。当前代码 fs.rs:328 已有检查，验证正确。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 4a, Blocked by Stage 3
  **References**: os/src/syscall/fs.rs:328 O_DIRECTORY check
  **Acceptance Criteria**: open(file, O_DIRECTORY) -> ENOTDIR. LTP open11 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=open11, expect TPASS. Evidence: stage4-enotdir.txt

- [ ] 4.3 O_NOFOLLOW + final symlink 返回 ELOOP

  **What to do**: open_file_at 中 follow_final=false 时，若 vfs_lookup 返回 symlink，返回 ELOOP(-40)。当前代码 fs.rs:290-296 已有检查，验证正确。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 4a, Blocked by Stage 3
  **References**: os/src/syscall/fs.rs:290-296 ELOOP check
  **Acceptance Criteria**: open(symlink, O_NOFOLLOW) -> ELOOP. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with inline test. Evidence: stage4-eloop.txt

- [ ] 4.4 rmdir 非空目录返回 ENOTEMPTY

  **What to do**: IndexNode::rmdir 中检查目录是否为空。非空 -> ENOTEMPTY(-39)。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 4b, Blocked by Stage 3
  **References**: os/src/fs/vfs/index_node.rs:212 rmdir, os/src/fs/ramfs/mod.rs rmdir impl
  **Acceptance Criteria**: rmdir non-empty -> ENOTEMPTY. LTP rmdir02,rmdir04 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=rmdir02,rmdir04, expect TPASS. Evidence: stage4-rmdir.txt

- [ ] 4.5 mknod/mknodat 权限检查 + mkdir errno

  **What to do**: sys_mknodat 中非 root -> EPERM。sys_mkdirat 中验证 parent 搜索+写权限。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 4b, Blocked by Stage 3
  **References**: os/src/syscall/fs.rs sys_mknodat, sys_mkdirat
  **Acceptance Criteria**: non-root mknod -> EPERM. LTP mknod01,mknod02,mkdir05,mkdir08 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=mknod01,mknod02,mkdir05,mkdir08, expect TPASS. Evidence: stage4-mknod.txt

---

#### Stage 5: hardlink、symlink、linkat、readlink

- [ ] 5.1 禁止目录 hardlink (EPERM)

  **What to do**: sys_linkat 中获取 old_inode metadata，若 file_type==Dir -> EPERM(-1)。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 5a, Blocked by Stage 4
  **References**: os/src/syscall/fs.rs:4958 sys_linkat, linux man 2 link
  **Acceptance Criteria**: link(dir, newname) -> EPERM. LTP linkat01,link04,link08 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=linkat01,link04,link08, expect TPASS. Evidence: stage5-linkdir.txt

- [ ] 5.2 link 成功后 inode nlink 增加 + 符号链接创建

  **What to do**: IndexNode::link 实现中增加 metadata.nlinks。symlink 创建验证权限和 errno。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 5a, Blocked by Stage 4
  **References**: os/src/fs/ext4/ext4fs.rs link, os/src/fs/ramfs/mod.rs link
  **Acceptance Criteria**: link->nlink+1; unlink->nlink-1. LTP link01,linkat02,symlink01,symlink03 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=link01,linkat02,symlink01,symlink03, expect TPASS. Evidence: stage5-nlink.txt

- [ ] 5.3 readlink 不追加 null, buffer 太小时截断

  **What to do**: readlinkat -> IndexNode::read_at 读取 symlink 内容。确保不追加 \0。bufsiz 小时返回实际写入长度（非 bufsiz）。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 5a (with 5.1, 5.2), Blocked by Stage 4
  **References**: os/src/syscall/fs.rs sys_readlinkat, linux man 2 readlink — "readlink() does not append a null byte to buf"
  **Acceptance Criteria**: readlink content = exact symlink target (no extra \0 byte). buf=1 on symlink target "abc" -> returns 1, writes 'a'. LTP readlink03 specific subcase: "readlink() with buffer size smaller than link content length". rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=readlink03, expect TPASS. Evidence: stage5-readlink.txt
  **References**: os/src/syscall/fs.rs sys_readlinkat, linux man 2 readlink
  **Acceptance Criteria**: readlink content = exact symlink target (no extra \0). LTP readlink03,readlinkat01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=readlink03,readlinkat01, expect TPASS. Evidence: stage5-readlink.txt

---

#### Stage 6: rename/renameat/renameat2

- [ ] 6.1 文件不能覆盖目录，目录不能覆盖文件

  **What to do**: sys_renameat2 中检查 old/new 类型。file->dir=EISDIR, dir->file=ENOTDIR。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 6a, Blocked by Stage 5
  **References**: os/src/syscall/fs.rs:2574 sys_renameat2, linux man 2 rename
  **Acceptance Criteria**: rename(file,dir)->EISDIR; rename(dir,file)->ENOTDIR. LTP rename04,05,06 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=rename04,rename05,rename06, expect TPASS. Evidence: stage6-types.txt

- [ ] 6.2 非空目录不能被覆盖 + RENAME_NOREPLACE

  **What to do**: new=dir 时检查非空 -> ENOTEMPTY。RENAME_NOREPLACE + new exists -> EEXIST。RENAME_EXCHANGE 不支持 -> EINVAL。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 6a, Blocked by Stage 5
  **References**: os/src/syscall/fs.rs:2574 sys_renameat2, linux man 2 renameat2
  **Acceptance Criteria**: rename(emptydir,nonempty)->ENOTEMPTY. renameat2(NOREPLACE) to existing->EEXIST. LTP rename07,12,renameat201,202 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=rename07,rename12,renameat201,renameat202, expect TPASS. Evidence: stage6-flags.txt

- [ ] 6.3 rename 后父目录 mtime/ctime 更新

  **What to do**: IndexNode::rename 完成后更新 old_parent 和 new_parent 的 mtime/ctime。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 6a (with 6.1, 6.2), Blocked by Stage 5
  **References**: os/src/fs/vfs/index_node.rs:193 rename, linux man 2 rename — "The file's ctime, as well as the mtime and ctime of each parent directory, shall be marked for update."
  **Acceptance Criteria**: After rename, old_parent.mtime/ctime and new_parent.mtime/ctime updated to current time. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=renameat01, verify timestamp updates. Evidence: stage6-mtime.txt

---

#### Stage 7: stat/statx/statfs/statvfs、时间戳和元数据

- [ ] 7.1 修复 statx mask 返回合理值

  **What to do**: sys_statx 中正确设置 stx_mask=STATX_BASIC_STATS, 修复 mtime/ctime/blksize 字段。当前 metadata_to_statx 代码已存在 (fs.rs:248-267)。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 7a, Blocked by Stage 6
  **References**: os/src/syscall/fs.rs:1931 sys_statx, os/src/fs/layout.rs Statx
  **Acceptance Criteria**: statx returns BASIC_STATS. LTP statx03,statx05,statx06 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=statx03,statx05,statx06, expect TPASS. Evidence: stage7-statx.txt

- [ ] 7.2 chmod/chown/link/unlink/rename 后更新 ctime

  **What to do**: 各 syscall 在元数据修改后更新 inode ctime=now。通过 IndexNode::set_metadata 更新。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 7a, Blocked by Stage 3-6
  **References**: os/src/syscall/fs.rs:2172 chmod, 2246 chown, 4958 linkat, 2574 renameat2
  **Acceptance Criteria**: After chmod/chown/link/unlink/rename, stat shows updated ctime. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=stat01,stat02, expect correct ctime. Evidence: stage7-ctime.txt

- [ ] 7.3 statfs/statvfs 返回合理值 + stat metadata 一致性

  **What to do**: FileSystem::statfs 确保 f_type/f_bsize/f_blocks 非零。metadata_to_stat 验证所有字段正确映射。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 7b, Blocked by Stage 6
  **References**: os/src/fs/vfs/file_system.rs SuperBlock, os/src/syscall/fs.rs:227 metadata_to_stat
  **Acceptance Criteria**: statfs returns non-zero values. stat returns correct uid/gid/mode/size. LTP statfs01-03,statvfs01,stat02,fstat01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=statfs01,statfs02,statvfs01, expect TPASS. Evidence: stage7-statfs.txt

- [ ] 7.4 utimensat 时间戳设置验证

  **What to do**: 验证 sys_utimensat 正确设置 atime/mtime。检查 UTIME_NOW/UTIME_OMIT 语义。添加 owner 检查（非 owner 需要 root）。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 7b, Blocked by Stage 3
  **References**: os/src/syscall/fs.rs:3805 sys_utimensat
  **Acceptance Criteria**: utimensat correctly sets timestamps. LTP utime06,utimes01,futimesat01 TPASS. rv64 build PASS.
**QA**: kernel-dev_kernel_run with ltp_include=utime06,utimes01, expect TPASS. Evidence: stage7-utime.txt

---

### Milestone C: FS Syscall Expansion (Stage 8-11)

---

#### Stage 8: fsync/fdatasync/syncfs/fallocate/lseek/readahead/copy_file_range

- [ ] 8.1 fsync/fdatasync/sync/syncfs 基础语义

  **What to do**: sys_fsync 通过 IndexNode::sync 实现。sys_fdatasync -> IndexNode::datasync。sys_sync -> flush_all_page_caches。sys_syncfs: 当前返回 ENOSYS — 修改为对 fd 所在 FS 调用 sync，若 fd 无效则返回 EBADF（参考 linux man 2 syncfs: "syncfs() returns 0 on success; on error, it returns -1 and sets errno to indicate the error. EBADF: fd is not a valid file descriptor."）。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 8a (with 8.2), Blocked by Stage 7
  **References**: os/src/syscall/fs.rs sys_fsync/sys_fdatasync/sys_sync/sys_syncfs, os/src/fs/vfs/index_node.rs sync/datasync, linux man 2 syncfs
  **Acceptance Criteria**: fsync(regular_fd) -> success (0). fdatasync(regular_fd) -> success (0). syncfs(bad_fd) -> EBADF. LTP fsync01,fsync03,fsync04,fdatasync01,fdatasync03 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=fsync01,fsync03,fdatasync01,fdatasync03, expect TPASS. Evidence: stage8-fsync.txt

- [ ] 8.2 fallocate mode/offset/size 参数检查

  **What to do**: sys_fallocate 中验证: mode=0 (allocate) 或 PUNCH_HOLE|KEEP_SIZE。无效 mode -> EOPNOTSUPP(95)。非法 offset/size -> EINVAL(22)。非 regular file -> ENODEV(19)。当前代码 fs.rs:4825 已有部分实现。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 8a (with 8.1), Blocked by Stage 7
  **References**: os/src/syscall/fs.rs:4825 sys_fallocate, linux man 2 fallocate — EOPNOTSUPP: "mode is not supported"; EINVAL: "offset+len exceeds file size"; ENODEV: "fd does not refer to a regular file"
  **Acceptance Criteria**: fallocate(dir_fd) -> ENODEV. fallocate(file_fd, unsupported_mode) -> EOPNOTSUPP. LTP fallocate03,fallocate04 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=fallocate03,fallocate04, expect TPASS. Evidence: stage8-fallocate.txt

- [ ] 8.3 lseek SEEK_DATA/SEEK_HOLE + copy_file_range 参数检查

  **What to do**: sys_lseek: SEEK_DATA=3, SEEK_HOLE=4 — 对 non-sparse tmpfs，SEEK_HOLE 在 EOF 处返回 offset，SEEK_DATA 在<=EOF 处返回 offset。sys_copy_file_range: 验证 fd_in/fd_out 类型，flags 必须为 0。检查 offset 指针。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 8b (with 8.4), Blocked by Stage 7
  **References**: os/src/syscall/fs.rs sys_lseek, sys_copy_file_range, linux man 2 lseek — "ENXIO: SEEK_DATA specified and offset is at or beyond EOF"
  **Acceptance Criteria**: lseek SEEK_DATA beyond EOF -> ENXIO. lseek SEEK_HOLE at EOF -> returns offset. LTP lseek01,lseek07 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=lseek01,lseek07, expect TPASS. Evidence: stage8-lseek.txt

- [ ] 8.4 copy_file_range basic functionality

  **What to do**: Validate sys_copy_file_range parameter checks pass. Core copy logic: read from fd_in -> write to fd_out in kernel buffer loop.

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 8b (with 8.3), Blocked by Stage 7
  **References**: os/src/syscall/fs.rs sys_copy_file_range, linux man 2 copy_file_range
  **Acceptance Criteria**: copy_file_range between two regular files copies data correctly. LTP copy_file_range01,copy_file_range03 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=copy_file_range01,copy_file_range03, expect TPASS. Evidence: stage8-copyfr.txt

---

#### Stage 9: xattr 扩展属性 (user.* namespace only)

- [ ] 9.1 在 IndexNode trait 和 tmpfs/ramfs 中实现 xattr KV 存储

  **What to do**:
  - IndexNode trait 已有 getxattr/setxattr 方法（返回 ENOSYS）
  - 在 tmpfs/ramfs inode 中增加 BTreeMap<String, Vec<u8>> xattrs 字段
  - 实现 getxattr(name, buf): 查找 key，复制 value 到 buf，返回长度；属性不存在->ENODATA；buf 太小->ERANGE
  - 实现 setxattr(name, value): 存储 KV；name 太长->ERANGE；value 太大->E2BIG
  - 实现 listxattr: 返回 null-separated 名称列表
  - 实现 removexattr: 删除 key

  **Must NOT do**: 不支持 security.*, trusted.*, system.* namespace（全部返回 ENOTSUP）。不支持 ext4 xattr（ext4 的 xattr 需要磁盘格式变更）。

  **Recommended Agent Profile**: Category deep, Skills []
  **Parallelization**: Wave 9a, Blocked by Stage 7 (needs stable VFS)
  **References**: os/src/fs/vfs/index_node.rs:383-390 getxattr/setxattr, os/src/fs/tmpfs/mod.rs, os/src/fs/ramfs/mod.rs, linux man 2 setxattr

  **Acceptance Criteria**: setxattr/getxattr/listxattr/removexattr on user.* namespace work. security.*->ENOTSUP. LTP setxattr01-04,getxattr01-04,listxattr01-03,removexattr01-02 TPASS. rv64 build PASS.

  **QA**: kernel-dev_kernel_run with ltp_include=setxattr01,setxattr02,getxattr01,getxattr02, expect TPASS. Evidence: stage9-xattr.txt

- [ ] 9.2 注册 xattr syscall 并实现 f* 和 l* 变体

  **What to do**:
  - 在 os/src/syscall/syscall_id.rs 添加: SYSCALL_SETXATTR(5), LSETXATTR(6), FSETXATTR(7), GETXATTR(8), LGETXATTR(9), FGETXATTR(10), LISTXATTR(11), LLISTXATTR(12), FLISTXATTR(13), REMOVEXATTR(14), LREMOVEXATTR(15), FREMOVEXATTR(16) — 使用 RISC-V 实际 syscall 号
  - 在 os/src/syscall/mod.rs dispatch 中注册
  - 实现 handler 函数，l* 版用 AT_SYMLINK_NOFOLLOW，f* 版走 fd
  - 添加参数校验: bad fd->EBADF, bad addr->EFAULT, 无搜索权限->EACCES

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 9b (SEQUENTIAL after 9.1 — depends on xattr storage), Blocked by Stage 7
  **References**: os/src/syscall/mod.rs dispatch, os/src/syscall/syscall_id.rs, linux man 2 setxattr
  **Acceptance Criteria**: All 12 xattr syscalls registered. setxattr + getxattr roundtrip. LTP fsetxattr01,fgetxattr01,lsetxattr01,lgetxattr01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=fsetxattr01,fgetxattr01,lsetxattr01,lgetxattr01, expect TPASS. Evidence: stage9-xattr-syscalls.txt

---

#### Stage 10: procfs / proc-sys / sysfs / devfs 的 LTP 环境补齐

- [ ] 10.1 补齐 procfs 关键缺失文件

  **What to do**:
  - /proc/cmdline: 返回 "BOOT_IMAGE=kernel\n"
  - /proc/self/mountinfo: 复用 /proc/mounts，添加 mount ID 和 parent ID (格式: "mount_id parent_id major:minor root mount_point options - fs_type source super_options")
  - /proc/self/io: 返回 "rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\ncancelled_write_bytes: 0\n" (全零占位，LTP read_all_proc 接受此格式)
  - /proc/self/pagemap: 需要时可 seek/read，返回全零（LTP proc01 接受）

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 10 (with 10.2, 10.3), Blocked by Stage 7
  **References**: os/src/fs/procfs/mod.rs, os/src/fs/procfs/files/mod.rs, LTP proc01 source (verify accepted /proc paths), linux man 5 proc
  **Acceptance Criteria**: /proc/cmdline readable (non-empty). /proc/self/mountinfo parseable (6+ space-separated fields per line). /proc/self/io readable. LTP proc01,proc02,procfs01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=proc01,procfs01, expect TPASS. Evidence: stage10-procfs.txt

- [ ] 10.2 补齐 devfs 缺失设备 + sysfs 基本结构

  **What to do**: /sys/block 目录（空目录，避免 fsync/sync case TBROK）。/sys/dev 目录。/dev/full 已在 Stage 1.4 实现，此处验证可用。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 10 (with 10.1, 10.3), Blocked by Stage 7
  **References**: os/src/fs/sysfs/mod.rs
  **Acceptance Criteria**: /sys/block and /sys/dev exist. fsync04,sync01 no longer TBROK. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=fsync04,sync01, expect no TBROK. Evidence: stage10-devfs.txt

- [ ] 10.3 确保基础设备文件稳定性验证

  **What to do**: 验证 /dev/null (read=0,write=swallow), /dev/zero (read=zeros), /dev/urandom 在所有 LTP setup 中可用。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 10 (with 10.1, 10.2), Blocked by Stage 7
  **References**: os/src/fs/dev/mod.rs, kernel-dev_kernel_run
  **Acceptance Criteria**: All 3 device files accessible on every QEMU boot. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with inline test stat /dev/{null,zero,urandom}, expect all exist. Evidence: stage10-devstable.txt

---

#### Stage 11: mount / umount / chroot / namespace 基础语义

- [ ] 11.1 mount 参数校验和 errno

  **What to do**: sys_mount 中验证: 非特权用户 -> EPERM。bad source/target 指针 -> EFAULT。target 不存在 -> ENOENT。target 不是目录 -> ENOTDIR。不支持的 fs type -> ENODEV（当前代码可能已处理，验证）。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 11a, Blocked by Stage 10
  **References**: os/src/syscall/fs.rs:3301 sys_mount
  **Acceptance Criteria**: mount by non-root -> EPERM. mount unsupported fs -> ENODEV. LTP mount01-04,mount07 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=mount01,mount02,mount03,mount04, expect TPASS. Evidence: stage11-mount.txt

- [ ] 11.2 umount busy/not-mountpoint 检查

  **What to do**: sys_umount2 中: 目标不是 mountpoint -> EINVAL。mount busy -> EBUSY。验证 umount flags。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 11a, Blocked by Stage 10
  **References**: os/src/syscall/fs.rs:2788 sys_umount2
  **Acceptance Criteria**: umount non-mountpoint -> EINVAL. umount busy -> EBUSY. LTP umount01-03,umount2_01 TPASS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=umount01,umount02,umount03, expect TPASS. Evidence: stage11-umount.txt

- [ ] 11.3 readonly mount 强制执行

  **What to do**: 验证 MountFSInode::ensure_mount_writable 在所有写操作入口被调用。确保 open(O_CREAT)/link/unlink/rename/chmod 在 readonly mount 上返回 EROFS。验证 bind mount + readonly。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 11b, Blocked by Stage 10
  **References**: os/src/fs/vfs/mount.rs MountFSInode::ensure_mount_writable, MountFlags::RDONLY
  **Acceptance Criteria**: open(O_CREAT) on readonly -> EROFS. link on readonly -> EROFS. LTP linkat02 readonly scenario -> EROFS. rv64 build PASS.
  **QA**: kernel-dev_kernel_run with ltp_include=linkat02, expect EROFS on readonly. Evidence: stage11-ro.txt

- [ ] 11.4 chroot 和 mount namespace 交互 + setns 基础

  **What to do**: sys_setns 支持 fd 参数。对不支持的类型返回 EINVAL。sys_pivot_root 返回 ENOSYS。

  **Recommended Agent Profile**: Category quick, Skills []
  **Parallelization**: Wave 11b, Blocked by Stage 10
  **References**: os/src/syscall/fs.rs or process/ids.rs
  **Acceptance Criteria**: setns on unsupported ns -> EINVAL. pivot_root -> ENOSYS. LTP setns01,setns02,pivot_root01 TPASS or TCONF. rv64 build PASS.
**QA**: kernel-dev_kernel_run with ltp_include=setns01,setns02, expect reasonable errno. Evidence: stage11-ns.txt

---

### Milestone D: Deferred/Stretch (Stage 12-15) — OPTIONAL

> **Status: DEFERRED — NOT active tasks**. No agent profiles or wave assignments apply.
> These stages serve as **planning placeholders** for future work. Each requires explicit user approval and a separate planning session before execution.

#### Stage 12: flock/fcntl/lease 文件锁 [DEFERRED]

Placeholder tasks (not for execution):
- 12.1 flock 互斥和阻塞语义 — 需要重构全局 FLOCK_TABLE
- 12.2 fcntl record lock 冲突检测 — 需要 per-inode 锁表
- 12.3 close/fork 后锁释放 — 需要 fd 生命周期集成

#### Stage 13: ioctl/FIEMAP/loop/block/immutable flag [DEFERRED]

Placeholder tasks (not for execution):
- 13.1 ioctl 不支持项返回 ENOTTY
- 13.2 FS_IOC_GETFLAGS/SETFLAGS + immutable flag
- 13.3 FIEMAP 基础

#### Stage 14: sendfile/splice/tee/vmsplice [DEFERRED]

Placeholder tasks (not for execution):
- 14.1 sendfile offset 处理
- 14.2 splice pipe-to-pipe
- 14.3 vmsplice 基础

#### Stage 15: quota/swap/new mount API/fanotify/inotify [DEFERRED]

Placeholder tasks (not for execution):
- 15.1 inotify 基础 watch/事件队列
- 15.2 fanotify 返回 ENOSYS (不假成功)
- 15.3 quota/swap 返回合理 errno

---

## Final Verification Wave

- [ ] F1. **Plan Compliance Audit** — oracle

  Read the plan end-to-end. For each "Must Have": verify implementation exists. For each "Must NOT Have": search codebase for forbidden patterns. Check evidence files exist. Compare deliverables against plan.

  **Output**: Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT

- [ ] F2. **Code Quality Review** — unspecified-high

  Run rv64 + la64 kernel build. Verify no panics. Check for: as any/@ts-ignore, empty catches, console.log, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.

  **Output**: Build [PASS/FAIL] | Lint [N clean/N issues] | VERDICT

- [ ] F3. **Real Manual QA** — unspecified-high

  Run ALL stage-level acceptance case lists through kernel-dev_kernel_run. Verify each target LTP case TPASS. Check regression set ~50 TPASS preserved. Verify no kernel panic.

  **Output**: Scenarios [N/N pass] | Regression [N/N preserved] | Panic [NONE] | VERDICT

- [ ] F4. **Scope Fidelity Check** — deep

  For each task: read what was planned, read actual git diff. Verify 1:1 mapping. Check "Must NOT do" compliance. Detect cross-task contamination. Flag unaccounted changes.

  **Output**: Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT

---

## Commit Strategy

| Stage | Commit Message Pattern |
|-------|----------------------|
| 0 | `docs(fs): update LTP baseline status and failure analysis` |
| 1 | `fix(fs): ensure rootfs has correct /etc/passwd, /etc/group, /tmp 1777, /dev/full` |
| 2 | `fix(vfs): path resolution errno fixes (ENOTDIR, ENOENT, EBADF, ELOOP)` |
| 3 | `fix(fs): implement umask, add chmod/chown permission checks, sticky bit enforcement` |
| 4 | `fix(fs): open/mkdir/rmdir/mknod flag validation and errno fixes` |
| 5 | `fix(fs): link/symlink/readlink semantic fixes (dir hardlink, nlink, readlink truncation)` |
| 6 | `fix(fs): rename type-checking, RENAME_NOREPLACE, non-empty dir protection` |
| 7 | `fix(fs): statx mask fix, ctime updates, statfs fixes, utimensat checks` |
| 8 | `fix(fs): fsync/fdatasync/syncfs, fallocate param checks, lseek, copy_file_range` |
| 9 | `feat(fs): add xattr user.* namespace support in tmpfs/ramfs` |
| 10 | `fix(fs): procfs/devfs/sysfs gap filling for LTP environment` |
| 11 | `fix(fs): mount/umount errno, readonly enforcement, chroot checks` |

**Pre-commit verification** (each commit):
```bash
make -C os rv64-kernel-build-only   # must PASS
# Milestone boundaries also:
make -C os la64-kernel-build-only   # must PASS
```

---

## Success Criteria

### Verification Commands
```bash
# Per stage: build + LTP run
make -C os rv64-kernel-build-only
kernel-dev_kernel_run(arch="rv64", timeout=300)

# Regression verification
kernel-dev_kernel_test_config(arch="rv64", mask="0x800", ltp_runner="inline",
  ltp_include="<regression-set>+<stage-targets>", ltp_libc="glibc", ltp_suites="syscalls")
kernel-dev_kernel_run(arch="rv64", log="off", timeout=300)

# Milestone boundary: la64 full build
make -C os la64-kernel-build-only
make -C os la64-run LOG=off
```

### Final Checklist
- [ ] All Must Have stages (0-7) complete with Oracle approval
- [ ] All Must NOT Have guardrails observed (no hardcoding, no test modification, no fake success)
- [ ] Regression set ~50 TPASS preserved throughout
- [ ] No kernel panic on any LTP run
- [ ] rv64 kernel build PASS at every commit
- [ ] la64 kernel build PASS at each milestone boundary
- [ ] Doc/Work_Log.md updated after every stage
- [ ] Doc/ltp/ltp_fs_status.md updated with final baseline
- [ ] All evidence files in .sisyphus/evidence/

```

(End of plan — all sections complete)
