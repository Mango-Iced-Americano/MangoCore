# 2026-05-22 LTP 全量适配推进记录

## 本轮目标

- `os_test.conf` 切到全量 LTP：`mask=0x800`，`ltp_include=` 置空。
- 优先修复非 fs/net、短平快、能直接提升 LTP 通过数的项目。
- fs/net、环境缺失、已知大面问题、卡死或耗时过长项目先加入 exclude，避免阻塞后续扫描。

## 已完成修复

### 1. `RLIMIT_STACK` ABI 状态

问题：`exec*` 类用例会读写 `RLIMIT_STACK`，原实现对栈限制修改处理不完整。

处理：
- 在 `TaskControlBlockInner` 中保存 `stack_limit_cur/max`。
- `clone` 时继承父任务 stack limit。
- `prlimit(RLIMIT_STACK)` 支持读写 ABI 可见状态；当前用户栈映射仍保持固定槽位，不动态重映射。

验证日志：
- `logs/ltp-20260522-adapt/rv64-exec-rlimit.log`
- `logs/ltp-20260522-adapt/la64-exec-rlimit.log`

结果：rv64/la64、musl/glibc 下 `execl01,execle01,execlp01,execv01,execve01` 均为真实 TPASS。

### 2. `eventfd/eventfd2`

问题：`eventfd*` 用例之前缺 `eventfd2(19)`，相关用例直接 ENOSYS。

处理：
- 新增 `fs/eventfd.rs`。
- 实现 `eventfd2(initval, flags)`，支持 `EFD_SEMAPHORE`、`EFD_NONBLOCK`、`EFD_CLOEXEC`。
- 实现 8 字节 read/write、计数器溢出、poll/epoll wait queue 语义。

验证日志：
- `logs/ltp-20260522-adapt/rv64-eventfd.log`
- `logs/ltp-20260522-adapt/la64-eventfd.log`

结果：
- `eventfd01-05`、`eventfd2_01-03` 在双架构真实 TPASS。
- `eventfd06` 为 `libaio is not available`，属于环境 TCONF，不是内核失败。

### 3. `clone301`: `CLONE_PIDFD` + `pidfd_send_signal`

问题：
- `clone3(CLONE_PIDFD)` 只验证了用户指针，没有创建 pidfd，也没有写回 fd。
- syscall 424 `pidfd_send_signal` 缺失，glibc 下 `clone301` 报 `ENOSYS`，随后子进程收不到信号。
- 补 pidfd 后又暴露 `siginfo.si_value` 未传递，LTP 期望 777，实际为 0。

处理：
- 新增匿名 `PidFd` inode，记录目标 pid。
- `clone3(CLONE_PIDFD)` 创建 pidfd 并写回用户 `args.pidfd`。
- 老 `clone(CLONE_PIDFD)` 路径按 Linux 语义使用 `ptid` 写回，且与 `CLONE_PARENT_SETTID` 冲突时返回 `EINVAL`。
- 新增 syscall 424 `pidfd_send_signal(pidfd, sig, info, flags)`。
- `SigInfo` 增加 `si_value` 字段，`pidfd_send_signal` 在用户传入 `siginfo_t` 时保留 payload 并入队。

验证日志：
- 修复前：`logs/ltp-20260522-adapt/rv64-clone301-info.log`
- pidfd 初修：`logs/ltp-20260522-adapt/rv64-clone301-after-pidfd.log`
- 最终验证：
  - `logs/ltp-20260522-adapt/rv64-clone301-after-siginfo.log`
  - `logs/ltp-20260522-adapt/la64-clone301-after-siginfo.log`

结果：
- rv64/la64、musl/glibc 均为 7 个 TPASS，内部 summary `failed 0`。
- 外层仍打印 `FAIL LTP CASE clone301 : 0`，这是现有 runner 包装行，不能按失败看。

### 4. `personality(2)` 最小 ABI

问题：`cve-2016-10044` 进入用例后首先调用 `personality(92)`，原内核未分发该 syscall，导致用例在前置阶段报 unsupported。

处理：
- 新增 syscall 92 `personality`。
- 在 `TaskControlBlockInner` 保存 Linux personality ABI 状态。
- `clone` 继承父任务 personality。
- `personality(0xffffffff)` / `usize::MAX` 只读旧值，其他值更新低 32 位状态。

验证日志：
- `logs/ltp-20260522-adapt/rv64-personality.log`
- `logs/ltp-20260522-adapt/la64-personality.log`

结果：`personality(92)` 不再 unsupported；`cve-2016-10044` 后续停在 `io_setup` 缺失并 TCONF。AIO 属于当前跳过范围，本轮不继续展开。

### 5. `execve03` errno 对齐

问题：全量扫描中 `execve03` 有两个真实 TFAIL：
- 超长路径场景期望 `ENAMETOOLONG`，实际走 VFS lookup 后返回 `ENOENT`。
- 不可执行普通文件场景期望 `EACCES`，实际先读魔数后返回 `ENOEXEC`。

处理：
- `sys_execve` 入口按 `MAX_PATHLEN/NAME_MAX` 提前校验路径长度。
- 打开可执行文件后、读取 ELF/shebang 魔数前检查元数据：必须是普通文件，且至少具备一个执行位。
- shebang 解释器打开路径复用同一检查。

验证日志：
- `logs/ltp-20260522-adapt/rv64-execve03-after.log`
- `logs/ltp-20260522-adapt/la64-execve03-after.log`

结果：rv64/la64、musl/glibc 下 `execve03` 均为 6 个 TPASS，内部 summary `failed 0`。

## 本轮跳过项

已按规则加入 `os_test.conf` 的全量 exclude：

- 已知大面/耗时：`epoll-ltp`、`epoll_ctl*`、`epoll_pwait*`、`epoll_create*`。
- fs/VFS/mount/权限类：`chdir01`、`chmod05/06/07`、`chown04`、`chroot01-04`、`fs_bind*`、`fs_racer*`、`fsconfig*`、`fsmount*`、`fsopen*`、`fspick*`、`fsstress`、`cp*`、`copy_file_range*`、`dio*`、`dirty*`、`du01.sh`、`df01.sh`、`fanotify*`、`fdatasync*`、`flock*`、`xattr*` 等。
- net/协议/网络环境：`busy_poll*`、`can_*`、`check_icmp*`、`dns*`、`dhcp*`、`dccp*`、`broken_ip*`、`bind_noport01.sh` 等。
- 环境/TCONF/helper：`add_key*`、`af_alg*`、`aio*`、`cap_bounds*`、`check_keepcaps`、`check_envval`、`cleanup_lvm.sh`、`cacheflush01`、`endian_switch01`、`event_generator`、`data`、`datafiles`、`find_portbundle` 等。

注意：这些跳过项不是声明内核已经支持，而是为了遵守“非 fs/net 优先、卡死/长耗时跳过”的当前适配策略。

## 扫描现状

已进行的全量扫描日志：

- `logs/ltp-20260522-adapt/rv64-full-ltp-after-pidfd.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-after-skip2.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-after-skip3.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-after-personality-skip.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-after-execve03.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-from-clockgettime02.log`
- `logs/ltp-20260522-adapt/rv64-full-ltp-after-fsbind-skip.log`

扫描发现：
- `clone301` 已从真实失败变为双架构通过。
- `personality` 前置缺口已补齐，AIO `io_setup` 仍按环境/大面项跳过。
- `execve03` 已从真实 TFAIL 变为双架构通过。
- 当前影响扫描推进的主要是 fs/net/epoll/文件锁/xattr/环境 helper，不适合作为本轮优先目标。
- 继续往后扫描时，应在更新 exclude 后从全量配置继续跑，寻找 syscall/process/mm/time/signal 方向的真实 TFAIL。

## 下一步建议

1. 用更新后的 `os_test.conf` 重新注入 rv64/la64 镜像。
2. 继续跑全量 LTP 扫描，遇到 fs/net/epoll/环境项继续记录并跳过。
3. 优先适配后续出现的非 fs/net 真失败，例如：
   - 缺 syscall 且语义较小的 compat 项；
   - process/signal/time/mm 类错误码不一致；
   - `TFAIL` 明确指向单个 syscall 行为差异的项目。
