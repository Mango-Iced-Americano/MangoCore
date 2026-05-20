# MangoCore FS-LTP 分诊与推进计划

> 最后更新: 2026-05-20
> 状态: Phase 0 — 体系建设中
> Oracle 审查: 已通过 (2026-05-20)

## 0. 核心原则

1. 先分类，再决定是否修复。只有 `FIXABLE_NOW` 才允许进入修复流程。
2. 不允许为单个 testcase 写硬编码 hack。不允许绕过 VFS/ext4/page cache/fd table 正常路径。
3. 不允许看到失败就直接改内核。每次修复前必须回答 4 个问题（见 2.1 节）。
4. 不允许大规模重构，除非当前问题确实无法局部修复，且必须先写清楚设计理由。
5. 不允许修一个 testcase 导致已有 PASS testcase 回退。
6. 不允许另起炉灶重写测试系统，必须优先复用 os_test.conf 和 scripts/ 下已有机制。
7. 不允许处理网络 LTP 测例。除非某问题确实发生在 fd table / poll / wait queue 等共用基础设施中，且必须说明影响范围。
8. 每个失败 testcase 修复前必须回答：Linux 期望语义、MangoCore 当前行为、差异所在层次、分类为 FIXABLE_NOW 的理由。
9. **所有用户可触达路径不得 panic** — 未实现功能返回 errno，非法参数返回 EINVAL/EFAULT，不触发 kernel panic。
10. **每次只推一个 family**（如 open* → 全稳定 → read* → …），不跨 family 并行修。

---

## 1. 运行结果与行动分类（分离）

LTP 每个 testcase 有两层属性，必须分开记录：

### 1.1 运行结果（客观）
| 结果 | 含义 |
|------|------|
| `TPASS` | 测试通过 |
| `TFAIL` | 测试失败（语义不符合预期） |
| `TBROK` | 测试框架/环境损坏，无法继续 |
| `TCONF` | 测试不适用（缺少内核配置/特性） |
| `PANIC` | 触发 kernel panic |
| `TIMEOUT` | 超时（单 case 或整轮） |

### 1.2 行动分类（人工决策）
| 分类 | 含义 | 动作 |
|------|------|------|
| **PASS** | 已通过，进入回归集 | 加入回归集，每次修复后回归 |
| **FIXABLE_NOW** | 当前 round 应支持，是 MangoCore 语义 bug | 允许修复，修复前必须回答 4 个问题 |
| **FIXABLE_LATER** | 未来应支持，依赖前置能力 | 暂不修，写清依赖 |
| **UNSUPPORTED** | 特性过重/性价比低/比赛不支持 | 加入 exclude，写清原因 |
| **ENV_FAIL** | LTP 环境/rootfs/shell/libc 问题 | 优先修环境，不误判为内核 bug |
| **DANGEROUS_STRESS** | 压力/破坏性/长时间测试 | 基础阶段禁止运行，后期单独隔离 |

---

## 2. 失败诊断流程

### 2.1 修复前必须回答的 4 个问题
1. 这个 testcase 在验证什么 Linux 语义？
2. 这个语义对 MangoCore 当前比赛目标是否必要？
3. 当前失败属于以下哪一层：
   - **A: syscall 参数/errno 语义** — syscall 入口参数校验、errno 返回
   - **B: fd table 生命周期** — fd 分配/释放/dup/clone 共享
   - **C: VFS path 解析** — openat/name_to_handle/vfs_lookup 路径遍历
   - **D: dentry/inode 生命周期** — 目录项缓存、inode 引用计数、unlink 后仍可读写
   - **E: ext4 数据路径** — read/write/extent/page cache 数据读写
   - **F: ext4 元数据路径** — inode 分配/释放、目录项读写、link count、superblock
   - **G: page cache 一致性** — read/write 与 page cache 同步、脏页回写、truncate 失效
   - **H: block device / block cache / writeback** — 块设备读写、metadata 写回
   - **I: copy_from_user / copy_to_user** — 用户态内存访问权限
   - **J: LTP 环境问题** — libc / shell / rootfs / PATH / /proc 缺失
   - **K: MangoCore 暂不支持的 Linux 特性** — xattr/ACL/quota/namespace 等
   - **L: 压力测试导致 timeout** — 资源耗尽或死锁
4. 只有分类为 `FIXABLE_NOW` 的 testcase 才允许修。

### 2.2 常见误判提醒
- 很多早期 FS fail 不是 ext4 bug，而是 **`/tmp`、`/proc`、权限模型、uid/gid、shell 工具、libc wrapper** 问题
- `TFAIL` 不等于"内核 bug" — 可能是 ENV_FAIL 或 UNSUPPORTED
- `TBROK` 往往是环境问题 — 优先排查 PATH/LTPROOT/rootfs/chmod
- `TCONF` 是预期内的"不适用" — 不需要修

---

## 3. Round 选择规则：硬门禁 + 评分

### 3.1 硬门禁（先于评分，一票否决）
以下类型**无论评分多少，一律不允许进入基础 round**：

| 门禁类型 | 处理方式 |
|----------|----------|
| 依赖 namespace/cgroup/security | 排除，标注 UNSUPPORTED |
| 依赖 xattr/ACL/quota | 排除，标注 UNSUPPORTED |
| 依赖 direct I/O (O_DIRECT) | 排除，标注 UNSUPPORTED |
| 依赖 mount namespace/propagation | 排除，标注 UNSUPPORTED |
| 依赖 CAP_SYS_ADMIN | 排除，标注 UNSUPPORTED 或 FIXABLE_LATER |
| 压力/破坏性/长时间测试 | 移入 DANGEROUS_STRESS |
| 网络相关测例 | 不在 FS-LTP 范围内，不处理 |
| 需要 Linux 5.x+ 的 syscall | 排除，标注 UNSUPPORTED（比赛内核基线 4.x语义） |

### 3.2 评分规则（通过硬门禁后）

**加分项**：
| 评分项 | 分数 |
|--------|------|
| 基础 syscall 语义 (open/read/write/close/stat/lseek) | +5 |
| 直接覆盖当前 FS round 核心目标 | +5 |
| 能影响 busybox/shell/LTP 大量后续测试 | +4 |
| 失败日志清晰 | +3 |
| 只依赖普通 ext4/rootfs | +2 |
| 能暴露 VFS/fd/inode/page cache 架构问题 | +3 |

**减分项**：
| 评分项 | 分数 |
|--------|------|
| 依赖复杂权限/capability（非 root 用户） | -3 |
| 依赖未实现 Linux 扩展 flag | -2 |
| 需要特殊文件类型（fifo/设备节点） | -2 |
| 会大量写盘或破坏镜像状态 | -3 |
| 测试框架复杂度高（需多进程精确同步） | -2 |

### 3.3 选择规则
- **priority >= 6**: 进入当前 round include
- **priority 3~5**: 放入 FIXABLE_LATER，说明原因
- **priority <= 2**: 放入 UNSUPPORTED 或 DANGEROUS_STRESS，说明原因

---

## 4. FS Round 设计

### 4.0 FS-Preflight: No-Panic 契约 + Runner 验证

> **目标**: 先保证"case 可以失败但不能打崩内核"，不修任何功能。

**验证项**:
1. LTP inline runner 可以连续跑基础白名单，单 case timeout 后能继续后续 case
2. ltp_include/ltp_exclude/ltp_from 配置机制生效
3. PANIC/TIMEOUT 检测和隔离正确工作
4. 镜像恢复机制正常（每轮或每次修复后镜像状态可控）
5. 用户可触达路径 `panic!()/todo!()/unwrap()` 基本清零（关注 `Doc/LTP_BOTTOM_UP_GUIDE.md` 中 P0 风险点）
6. 未实现功能返回明确 errno（ENOSYS/EINVAL/EOPNOTSUPP），不触发 kernel panic

**通过标准**:
- LTP inline runner 连续跑 3 轮预备白名单，无 kernel panic
- 超时 case 正确被 skip，后续 case 不受影响
- 支持断点续跑（`ltp_from` 机制）

**本轮不修任何 testcase 语义。** 只修 panic/todo/unwrap。

---

### 4.1 FS-Round-0: VFS / fd / path / 基础读写语义

> **目标**: 先保证普通用户程序最基本的文件访问路径稳定。

**核心 family（必须全部稳定才能晋级）**:
| Family | 测例数 | 来源 | 说明 |
|--------|--------|------|------|
| open/openat | ~18 | LTP syscalls/open + openat | 文件打开基础 |
| close | ~4 | LTP syscalls/close + close_range | 文件关闭 |
| read | 4 | LTP syscalls/read (read01-04) | 基础读取 |
| write | 6 | LTP syscalls/write (write01-06) | 基础写入 |
| lseek | ~11 | LTP syscalls/lseek | 文件偏移 |
| stat/fstat/lstat | ~9 | LTP syscalls/stat + fstat + lstat | 文件元数据 |
| access/faccessat | ~9 | LTP syscalls/access + faccessat | 访问权限检查 |
| getcwd/chdir | ~10 | LTP syscalls/getcwd + chdir + fchdir | 工作目录 |
| getdents/getdents64 | ~4 | LTP syscalls/getdents + readdir | 目录读取 |
| dup/dup2/dup3 | ~16 | LTP syscalls/dup + dup2 + dup3 | fd 复制 |
| fcntl 基础项 | ~14 | fcntl01-14（不含 OFD 锁） | F_DUPFD/F_GETFD/F_SETFD/F_GETFL/F_SETFL |

**辅助 family（有则跑，但不作为晋级硬门禁）**:
| Family | 说明 | 备注 |
|--------|------|------|
| pipe/pipe2 | ~18 测例，与读写路径共享 fd/File 基础设施 | 确认基础 pipe 路径可用 |
| creat/open(O_CREAT) | 已验证通过(ltp_include)，保留为种子 | 不阻塞晋级 |
| pread/pwrite | 基础 offset 语义 smoke | 完整一致性验证在 Round-2 |

**进入本轮条件**:
1. FS-Preflight 通过
2. 不依赖 xattr/ACL/quota/namespace/direct I/O
3. 不依赖 mount 传播
4. 不依赖并发压力
5. 主要验证 VFS / path / fd table / inode / read / write 基础语义

**本轮排除（不参与 Round-0 通过率计算）**:
- fchmod/chown/fchownat — 权限/所有者修改，属于 Round-1
- fallocate — 空间预分配，属于 Round-2
- flock — 文件锁，属于 Round-1 或 UNSUPPORTED
- creat — 已作为辅助种子存在

**本轮 FIXABLE_NOW 示例**:
- open errno 不符合预期
- close 后 fd 生命周期错误
- read/write 返回值错误
- file offset 更新错误
- lseek 语义错误
- stat/fstat/lstat 基础字段明显错误
- access/faccessat 基础行为错误
- getcwd/chdir 路径状态错误
- getdents 目录项读取错误
- dup/dup2/dup3 共享 file offset 或 fd 生命周期错误
- fcntl 基础 flag 行为错误

### 4.2 FS-Round-1: 目录修改和 ext4 metadata

> **目标**: 在基础 VFS 读写稳定后，开始验证目录项、inode 元数据、link count、truncate、rename 等写路径。

**核心 family**:
| Family | 测例数 | 说明 |
|--------|--------|------|
| mkdir/mkdirat | ~7 | 目录创建 |
| rmdir | ~3 | 目录删除 |
| unlink/unlinkat | ~6 | 文件删除 |
| rename/renameat | ~16 | 重命名（不含 renameat2 复杂 flag） |
| link/linkat | ~6 | 硬链接 |
| symlink/symlinkat | ~4 | 符号链接 |
| readlink/readlinkat | ~4 | 读取符号链接 |
| truncate/ftruncate | ~5 | 文件截断 |
| utime/utimes/utimensat | ~10 | 时间戳（基础精度，不要求极端） |
| chmod/fchmod/fchmodat | ~15 | 权限修改 |
| chown/fchown/fchownat | ~15 | 所有者修改 |

**进入本轮条件**:
1. FS-Round-0 全部核心 family 稳定
2. fd/inode/dentry 生命周期没有明显 panic 或泄漏
3. ext4 基础 read/write 已经可靠
4. page cache 基础读写路径至少不会明显破坏普通 read/write

**本轮 FIXABLE_LATER 示例**:
- sticky bit
- setuid/setgid 复杂行为
- renameat2 复杂 flag (RENAME_EXCHANGE/RENAME_NOREPLACE)
- 高精度时间戳 corner case
- 跨文件系统 hardlink

### 4.3 FS-Round-2: page cache / ext4 一致性 / file-backed mmap

> **目标**: 验证普通文件 I/O、page cache、ext4 数据路径、文件映射之间的一致性。

**核心 family**:
| Family | 测例数 | 说明 |
|--------|--------|------|
| pread/pwrite (完整语义) | ~6 | 定位读写 offset 不改变 |
| readv/writev | ~8 | 向量 I/O scatter/gather |
| fsync/fdatasync | ~7 | 文件同步 |
| sync/syncfs | ~2 | 全局同步 |
| mmap file-backed | ~12 | 文件映射基础（不含 MAP_HUGETLB） |
| msync | ~4 | 内存同步 |
| fallocate 基础 | ~6 | 空间预分配基础模式 |
| truncate 与 page cache 交互 | — | 截断后 page cache 正确失效 |

**进入本轮条件**:
1. FS-Round-0 和 FS-Round-1 全部核心 family 稳定
2. 普通 read/write/truncate 不再频繁出现基础语义错误
3. page cache 与 ext4 read/write 路径已经明确

**本轮 FIXABLE_LATER 示例**:
- 完整 direct I/O
- 完整零拷贝 sendfile/splice 优化
- fallocate 复杂 flag
- MAP_HUGETLB/MLOCK 等高级 mmap flag

### 4.4 FS-Round-3: 高级 I/O + 稳定性

> **目标**: 在基础语义稳定后，验证 sendfile/splice/copy_file_range 等复杂 I/O，以及压力测试。

**候选**: sendfile*, splice*, copy_file_range*, fsx, fsstress, fs_racer, read_all, doio

**进入本轮条件**:
1. FS-Round-0/1/2 回归稳定
2. 已经有 panic/timeout 隔离机制
3. 每个 stress testcase 必须单独运行，必须有硬超时
4. stress 只用于发现稳定性问题，不用于定义基础语义

### 4.5 长期排除（不进入任何 Round）

| 类别 | 测例数(约) | 原因 |
|------|-----------|------|
| xattr 系列 | ~32 | 扩展属性，比赛内核范围外 |
| ACL 系列 | ~1 | 访问控制列表 |
| quota 系列 | ~9 | 磁盘配额 |
| namespace 相关 | ~7 | 命名空间隔离 |
| mount propagation | ~85 | 挂载传播（fs_bind 系列） |
| fanotify | ~25 | 文件通知框架 |
| inotify | ~14 | inode 通知框架 |
| chroot/pivot_root | ~5 | 根目录切换 |
| landlock | ~10 | Linux 5.13+ 安全模块 |
| io_uring | ~3 | Linux 5.1+ 异步 I/O |
| userfaultfd | ~6 | Linux 4.3+ |
| statmount/listmount | ~13 | Linux 6.8+ |
| fsconfig/fsmount/fsopen | ~9 | Linux 5.2+ |
| openat2/close_range 等 | ~7 | Linux 5.x+ |
| file_attr (chattr/lsattr) | ~5 | 需要 ext2/3/4 特殊 ioctl |

---

## 5. 晋级规则

### 5.1 晋级条件（全部满足）

1. **核心 family gate**: 当前 round 所有「核心 family」全部稳定（每个核心 family 中，至少排名前 2/3 的测例 PASS，且无 PANIC）
2. **无 kernel panic**: 最近一次完整 round 运行（所有核心 family + 辅助 family）无 kernel panic
3. **无 unexplained timeout**: 所有超时均已分类（DANGEROUS_STRESS / ENV_FAIL / FIXABLE_LATER）
4. **无 regression**: 已通过测例（回归集）无回退
5. **剩余 case 已分类**: 非 PASS 的 case 均已标注 FIXABLE_LATER / UNSUPPORTED / ENV_FAIL / DANGEROUS_STRESS
6. **文档已更新**: Doc/ltp_fs_status.md 已更新

### 5.2 辅助指标
- 通过率 >= 85% 可作为参考，但**不能替代核心 family gate**
- 如果通过率 >= 85% 但某个核心 family 仍有 FIXABLE_NOW → 不能晋级

### 5.3 Panic/Timeout 阻断
如果当前 round 还有 PANIC 或 unexplained TIMEOUT：
- **不能晋级**
- 必须先定位
- 如果确认是 DANGEROUS_STRESS → 移入 DANGEROUS_STRESS
- 如果确认是当前基础语义 bug → 改为 FIXABLE_NOW 并修复
- 如果确认是测试环境问题 → 改为 ENV_FAIL

---

## 6. 每轮工作流程

```
1. 从 LTP 上游列表出发，筛选当前 round 候选 family
2. 应用硬门禁排除（xattr/ACL/quota/namespace/direct I/O/stress/网络）
3. 对通过硬门禁的 family 做 priority 评分
4. 选出 priority >= 6 的 family → 当前 round 探索集
5. 将历史已 PASS 测例 → 回归集（独立于探索集，始终跑）
6. 将硬排除测例 → 强制排除集（不经评分）
7. 生成 os_test.conf: ltp_include = 探索集 + 回归集, ltp_exclude = 强制排除集
8. 运行当前 round（每次只跑一个 family，例如先 open* → 全稳定 → read* → …）
9. 解析每个 testcase 的运行结果（TPASS/TFAIL/TBROK/TCONF/PANIC/TIMEOUT）
10. 更新 Doc/ltp_fs_status.md（记录 arch/libc/run_id/log/结果/分类/失败层次）
11. 选择第一个 FIXABLE_NOW testcase
12. 阅读 testcase 源码或日志，说明它期望的 Linux 语义
13. 定位 MangoCore 当前行为与期望语义的差异（在哪一层？）
14. 只修根因，不修表象
15. 修复后运行：当前 testcase + 回归集全部
16. 如果出现 regression → 立即停止推进，优先修 regression
17. 如果当前 round 满足晋级条件 → 自动生成下一 round 的 include 列表
```

### 6.1 三列表分离原则

| 列表 | 来源 | 更新方式 | 用途 |
|------|------|----------|------|
| **回归集** | 历史已通过的测例 | 仅追加，永不删除（除非发现误判） | 每次修复后全量回归 |
| **探索集** | 当前 round 候选 family | 每 round 重新生成 | 本 round 要验证的测例 |
| **强制排除集** | 硬门禁排除 | 随 round 设计更新 | 确保不误跑危险/不支持测例 |

**os_test.conf 映射**:
- `ltp_include` = 回归集 + 探索集
- `ltp_exclude` + `ltp_exclude_musl` + `ltp_exclude_glibc` = 强制排除集

---

## 7. 现有基础设施（不重复造轮子）

| 工具/脚本 | 功能 | 复用方式 |
|-----------|------|----------|
| `os_test.conf` | ltp_include / ltp_exclude / ltp_exclude_musl / ltp_exclude_glibc / ltp_libc / ltp_from / ltp_runner | 直接读写配置字段 |
| `scripts/auto_include_ltp.py` | 多轮自动收集 TPASS 测例，支持断点续跑 | Phase 1 (运行) + Phase 2 (扫描) 两阶段架构可复用 |
| `scripts/auto_exclude_ltp.sh` | 自动排除 panic/timeout 测例 | 可复用 panic/timeout 检测逻辑 |
| `scripts/auto_exclude_glibc.py` | glibc 版 exclude 收集 | 可复用 glibc 排除逻辑 |
| `scripts/run_full_test.py` | 全量编译+测试+评分+归档 | 评分部分（judge/run_parse.py）可复用 |
| `user/src/bin/initproc.rs` | 内联 LTP runner，支持 include/exclude/from | 直接使用 `run_ltp_binaries()` |
| `judge/judge_ltp-musl.py` / `judge_ltp-glibc.py` | 解析 RUN/FAIL LTP CASE 输出 | 可用于结果解析 |
| `Doc/LTP_BOTTOM_UP_GUIDE.md` | 自底向上适配指导 | 参考 P0-P8 优先级和代码审计结论 |

---

## 8. FS-Round-0 核心 Family 测例清单

> 数据来源: LTP 上游仓库 (linux-test-project/ltp) `testcases/kernel/syscalls/` 完整扫描
> 总计 FS 相关 ~730+ 测例，Round-0 核心 family 约 ~100 测例

### 8.1 open / openat (18 测例, Priority: 10)

| 测例 | 说明 | 通过硬门禁 |
|------|------|-----------|
| open01-04 | 基础 open (O_RDONLY/O_WRONLY/O_RDWR/O_CREAT) | ✅ |
| open06-15 | 错误处理、O_TRUNC、O_APPEND、大文件、符号链接 | ✅ |
| openat01-04 | openat 基础 + fd 参数验证 | ✅ |

> 排除: openat2(01-03) — 需要 Linux 5.6+

### 8.2 close (4 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| close01-02 | 基础 close + close 后 fd 无效 |
| close_range01-02 | close_range — 需要 Linux 5.9+ (**排除**) |

> 实际: close01, close02

### 8.3 read (4 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| read01 | 基础 read |
| read02 | read 错误处理 (EBADF/EINVAL) |
| read03 | read 到只写 fd |
| read04 | 大文件/跨页 read |

### 8.4 write (6 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| write01 | 基础 write |
| write02 | write 错误处理 |
| write03 | write 到只读 fd |
| write04 | write 到管道 (EPIPE) |
| write05-06 | 大文件 write / 追加 |

### 8.5 lseek (11 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| lseek01 | 基础 SEEK_SET |
| lseek02 | SEEK_CUR |
| lseek03 | SEEK_END |
| lseek04-10 | 错误处理、管道 ESPIPE、大 offset |
| lseek11 | SEEK_DATA/SEEK_HOLE (可能 UNSUPPORTED) |

### 8.6 stat / fstat / lstat (9 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| stat01-04 | stat 基础: 权限/时间戳/size/nlink |
| fstat02-03 | fstat 基础 |
| lstat01-03 | lstat 基础 (符号链接不跟随) |
| fstatat01 | fstatat (newfstatat) |

> 排除: statx01-12 — 需要 Linux 4.11+，Round-2

### 8.7 access / faccessat (9 测例, Priority: 8)

| 测例 | 说明 |
|------|------|
| access01-04 | 基础 access (R_OK/W_OK/X_OK/F_OK) |
| faccessat01-02 | faccessat 基础 |
| faccessat201-202 | faccessat2 — 需要 Linux 5.8+ (**排除**) |

> 实际: access01-04, faccessat01-02

### 8.8 getcwd / chdir (10 测例, Priority: 8)

| 测例 | 说明 |
|------|------|
| getcwd01-04 | 基础 getcwd, 错误处理, 长路径 |
| chdir01-02, chdir04 | 基础 chdir |
| fchdir01-03 | fchdir 基础 |

### 8.9 getdents / readdir (4 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| getdents01-02 | 基础 getdents/getdents64 目录读取 |
| readdir01, readdir21 | readdir (libc 封装) |

### 8.10 dup / dup2 / dup3 (16 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| dup01-07 | dup 基础: 共享 offset, close-on-exec, 错误处理 |
| dup201-207 | dup2 基础: 指定 target fd, 关闭旧 fd |
| dup3_01-02 | dup3 基础 (O_CLOEXEC) |

### 8.11 fcntl 基础项 (14 测例, Priority: 10)

| 测例 | 说明 |
|------|------|
| fcntl01-05 | F_DUPFD (基础) |
| fcntl06-10 | F_GETFD/F_SETFD (close-on-exec) |
| fcntl11-14 | F_GETFL/F_SETFL (O_RDONLY/O_WRONLY/O_RDWR/O_APPEND) |

> 排除: fcntl15-40 — OFD 锁(需 3.15+)、seals(需 memfd)、pipe buffer(需 2.6.35+)

### 8.12 辅助 Family (不阻塞晋级)

| Family | 测例数 | 说明 |
|--------|--------|------|
| pipe/pipe2 | ~18 | 与 File/fd 共享基础路径 |
| creat | ~9 | 文件创建 (O_CREAT)，已有 3 个通过 |
| pread/pwrite | ~6 | 基础 offset 语义 smoke |
| umask | 1 | 最小权限掩码 |

### 8.13 Round-0 强制排除项

| 类别 | 测例数(约) | 原因 |
|------|-----------|------|
| xattr 系列 | ~32 | UNSUPPORTED |
| ACL | ~1 | UNSUPPORTED |
| quota | ~9 | UNSUPPORTED |
| namespace | ~7 | UNSUPPORTED |
| mount propagation (fs_bind) | ~85 | UNSUPPORTED |
| fanotify | ~25 | UNSUPPORTED |
| inotify | ~14 | UNSUPPORTED |
| direct I/O (diotest) | ~6 | UNSUPPORTED |
| fchmod/chown/fchownat | ~35 | Round-1，不参与 Round-0 通过率 |
| fallocate | ~6 | Round-2 |
| flock | ~6 | Round-1 或 UNSUPPORTED |
| fsstress/fsx/doio/racer | ~20 | DANGEROUS_STRESS |
| 网络相关（socket/bind/connect 等） | ~92 | 不在 FS-LTP 范围 |

---

## 9. 文档维护

- `Doc/ltp_fs_plan.md` — 本文档，阶段设计+规则
- `Doc/ltp_fs_status.md` — 每个 testcase 的实时状态表
- `os_test.conf` — include/exclude 配置（通过脚本更新）

每轮完成后必须更新 `Doc/ltp_fs_status.md`。

---

## 10. 附录：FS 内核模块快速索引

| 层 | 关键文件 | 行数 |
|----|----------|------|
| syscall 分发 | `os/src/syscall/fs.rs` | 2319 |
| syscall 注册 | `os/src/syscall/mod.rs` | ~500 |
| VFS 入口 | `os/src/fs/mod.rs` | ~300 |
| File (fd 层) | `os/src/fs/vfs/file.rs` | 1005 |
| IndexNode trait | `os/src/fs/vfs/index_node.rs` | 375 |
| FileSystem trait | `os/src/fs/vfs/file_system.rs` | 120 |
| MountFS | `os/src/fs/vfs/mount.rs` | 663 |
| PageCache | `os/src/fs/page_cache.rs` | 917 |
| ext4 主实现 | `os/src/fs/ext4/ext4fs.rs` | 1923 |
| ext4 inode | `os/src/fs/ext4/ext4_inode.rs` | ~1200 |
| ext4 extent | `os/src/fs/ext4/extent.rs` | ~400 |
| ext4 direntry | `os/src/fs/ext4/direntry.rs` | ~500 |
| BlockDevice trait | `os/src/drivers/block/block_dev.rs` | ~50 |
| DevFS | `os/src/fs/dev/mod.rs` | 203 |
| ProcFS | `os/src/fs/procfs/mod.rs` | ~200 |
| RamFS | `os/src/fs/ramfs/mod.rs` | 801 |
| FAT32 | `os/src/fs/fat32/` | ~1800 |
| poll 实现 | `os/src/fs/poll.rs` | 507 |
