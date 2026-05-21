# 2026-05-20 双架构全量测试与 task 重构效果记录

> 2026-05-21 追加：本报告保留 2026-05-20 全量测试的原始结论，同时补充 la64 `clone` ABI 与 signal-frame sigmask 对齐修复后的聚焦复测结果。原始全量日志仍以 `current-testresult/` 为准，新增复测结果用于更新问题状态和后续优先级。

## 1. 测试范围

- 仓库：`/Users/luzimo/dev/MangoCore`
- 分支：`refactor/task`
- HEAD：`b61a8fb task重构：收尾`
- 构建命令：`docker compose exec -T os-dev bash -lc 'make all'`
- 测试命令：`docker compose exec -T os-dev bash -lc 'TEST_ARCH=both GROUP_TIMEOUT_SEC=600 bash run_test.sh'`
- 说明：`run_test.sh` 依次跑 la64、rv64；每组外层超时 600s；2026-05-20 原始全量测试未改内核代码。2026-05-21 追加复测基于修复 `os/src/syscall/mod.rs` 的 la64 `SYSCALL_CLONE` 参数解码，以及修复 `os/src/hal/*/trap/context.rs`、`os/src/task/signal/mod.rs`、`os/src/syscall/process/signal.rs` 的 signal-frame sigmask 布局。

## 2. 日志位置

- 构建日志：`logs/full-test-20260520-task-refactor/build.log`
- 全量驱动日志：`logs/full-test-20260520-task-refactor/run_test_driver.log`
- 本轮 24 个测试组原始日志：`logs/full-test-20260520-task-refactor/current-testresult/{la,rv}/*.log`
- `logs/full-test-20260520-task-refactor/preexisting-testresult/` 是运行前的旧 `testresult` 备份。
- `logs/full-test-20260520-task-refactor/testresult/` 是运行后的完整快照，包含 `testresult/rv` 中原有的历史调试日志；本报告只以 `current-testresult/` 为本轮依据。

## 3. 总体结果

| 架构 | PASS | FAIL | TIMEOUT | TOTAL | 脚本结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| la64 | 5 | 5 | 2 | 12 | basic、lua、iozone、libcbench、cyclictest 通过；busybox、libctest、unixbench、lmbench、netperf 失败；iperf、ltp 超时 |
| rv64 | 5 | 2 | 5 | 12 | basic、lua、libctest、iozone、cyclictest 通过；busybox、libcbench 失败；unixbench、iperf、lmbench、netperf、ltp 超时 |
| 合计 | 10 | 7 | 7 | 24 | `run_test.sh` 最终退出码 1 |

注意：脚本 PASS 不等于子项真实 PASS。本次发现 `libcbench/la64`、`cyclictest/la64`、`cyclictest/rv64`、`libctest/rv64` 都有“组通过但内部有 fail/异常”的情况，后续统计必须继续看原始日志。

## 4. 构建结论

- `make all` 在 Docker 内完成，rv64 与 la64 串行构建均成功。
- 未出现编译错误。
- 构建日志中有大量 warning，最终 la64 `os` bin 统计为 105 条 warning；全日志重复编译阶段累计匹配到 `warning:` 1801 次。当前不是阻塞项，但后续可以单独清理。

## 5. 内核稳定性结论

本轮原始日志未匹配到以下内核致命类关键词：

- `panic` / `panicked`
- `HEAP ALLOCATION` / `alloc_error`
- `trap_from_kernel`
- `StorePageFault in kernel` / `PageFault in kernel`
- `not yet implemented` / `todo`

这说明 task 重构至少显著改善了“测试中直接把内核打崩”的问题。`iozone` 双架构均通过，也没有复现旧的 heap fatal / ext4 todo 类崩溃。当前主要问题从“内核存活性”转向“POSIX 语义、线程/fork、procfs/LTP 适配和网络兼容性”。

## 6. task 重构效果评估

### 正向效果

- 双架构完整启动、跑完整个测试序列，未出现内核 panic。
- rv64 `basic` 覆盖 clone/fork/wait/execve 等基础路径，通过。
- rv64 LTP 未出现 `fork(): EFAULT`，相比 la64 明显稳定。
- 旧问题 `/proc/<pid>/stat` 本轮未再出现 `cut: /proc/<pid>/stat: No such file or directory`；当前源码也已经在 `os/src/fs/procfs/pid/mod.rs` 注册 `stat`。
- iozone 双架构通过，说明文件 I/O 和内存压力至少没有再触发以前那类致命崩溃。

### 2026-05-20 原始仍未解决的问题

- la64 的 fork/clone 相关路径仍明显不稳：
  - `unixbench/la64` 多次 `Fork failed at iteration 0`，原因是 `Bad address`。
  - `ltp/la64` 出现 45 次 `fork(): EFAULT (14)`。
  - `cyclictest/la64` 的 hackbench 阶段出现 `fork() (error: Bad address)` 和 `Creating workers (error: Bad address)`。
- la64 pthread/TLS 路径仍有成片用户态异常：
  - `libctest/la64` 出现 138 次 `Exception(...) in application`，集中在 `pthread_cancel*`、`pthread_cond*`、`pthread_robust_detach`、`pthread_once_deadlock`、`tls_init`、`tls_local_exec`、`tls_get_new_dtv` 等用例。
  - 常见坏地址包括 `0x0`、`0x600040000`、`0xfffffffffffffff8`、`0xf600...`。
- rv64 没有 la64 那种 LTP fork EFAULT，但线程兼容性仍不完整：
  - `libcbench/rv64` 多次 unsupported syscall 435，后续 pthread benchmark 超时；435 在 Linux 语境下通常对应 `clone3`。
  - `libctest/rv64` 虽然脚本判定 PASS，但原始日志里有 pthread、scanf/locale、socket/stat、regex、setvbuf 等 FAIL，并有少量用户态 `InstructionPageFault`。

### 2026-05-21 追加：la64 clone ABI 修复后状态

已定位并修复 la64 fork/clone/TLS 大片异常的主因：LoongArch raw `clone` ABI 是 `flags, stack, ptid, ctid, tls`，而原 syscall 分发路径按 rv64/通用顺序 `flags, stack, ptid, tls, ctid` 传给 `sys_clone`。这会把用户态 `ctid` 当作 TLS，把 `tls` 当作 child-tid 地址，直接导致 `CLONE_CHILD_SETTID` 写错地址、TLS 指针错乱和后续 pthread 崩溃。

修复点：

- `os/src/syscall/mod.rs`
- 非 la64 保持原参数顺序：`flags, stack, ptid, tls=args[3], ctid=args[4]`
- la64 单独解码：`flags, stack, ptid, tls=args[4], ctid=args[3]`

复测结果：

- 双架构编译通过：
  - `docker compose exec os-dev make -C os rv64-kernel-build-only`
  - `docker compose exec os-dev make -C os la64-kernel-build-only`
- la64 LTP 聚焦用例 `abort01,accept01,accept02,accept03,bind04,bind05` 不再出现 `fork(): EFAULT`。
- la64 libctest 中原先成片 TLS 错位已明显收敛：`tls_init`、`tls_local_exec`、`tls_get_new_dtv`、`pthread_cond*`、`pthread_once_deadlock` 等已能通过。剩余失败集中在 `pthread_cancel*` 的 `0x0` 取指、glibc `unknown syscall 435`、locale/socket/stdio 等独立语义问题。
- la64 cyclictest 的 hackbench 阶段不再报 `Bad address`，现失败变为 `Resource temporarily unavailable`，说明 fork 参数错位已解除，剩余是任务资源上限、线程回收或 fd/worker 创建压力问题。
- la64 unixbench 已跑过 SPAWN/EXECL 等 fork/exec 压力项，不再出现 `Fork failed at iteration 0 / Bad address`；仍存在 timeout/性能问题。

更新结论：task 重构已经解决了“内核存活性”问题；la64 `clone` ABI 修复又消除了 fork EFAULT/TLS 指针错位这一主阻塞。下一阶段不应继续把 la64 fork Bad address 当作首要根因，而应转向资源上限、pthread_cancel/信号返回、clone3/syscall 435、scheduler syscall 236/120/121 和 procfs/network 兼容。

### 2026-05-21 追加：la64 signal-frame sigmask 对齐修复后状态

在 `clone` ABI 修复后，la64 `pthread_cancel*` 仍集中触发 `PageInvalidFetch bad addr = 0x0`。进一步定位后确认主因不是 pthread TLS 本身，而是 la64 signal frame 的 `sigmask` 布局不稳定：`Signals` 底层是 `u128`，直接放入 `#[repr(C)] UserContext` 会引入 16 字节对齐 padding；但 `do_signal()` 和 `sys_sigreturn()` 手工计算 frame 偏移时按无额外 padding 的布局读写，导致 `MachineContext` 恢复地址错位，最终把用户态 `pc/ra` 恢复坏。

修复点：

- `os/src/hal/arch/loongarch64/trap/context.rs`：新增 `UserSignalMask { bits: [usize; 2] }`，用用户 ABI 稳定布局存储 signal mask。
- `os/src/hal/arch/riscv/trap/context.rs`：新增 `UserSignalMask { bits: [usize; 1] }`，保持通用 signal 代码双架构可编译且不改变 rv64 frame 尺寸。
- `os/src/task/signal/mod.rs`：signal frame 构造统一走 `UserContext::new()` / `UserContext::encode_sigmask()`，非 `SA_SIGINFO` 路径的 `mcontext` 偏移改用 `size_of::<UserSignalMask>()`，并避免 `SA_RESTORER` 标志存在但 restorer 为 0 时跳到空地址。
- `os/src/syscall/process/signal.rs`：`sigreturn` 按 `UserSignalMask` 读取和解码旧 signal mask，恢复 `MachineContext` 的偏移与构造端一致。

复测结果：

- 双架构编译通过：
  - `docker compose exec os-dev make -C os rv64-kernel-build-only`
  - `docker compose exec os-dev make -C os la64-kernel-build-only`
- la64 musl focused `pthread_cancel`、`pthread_cancel_sem_wait` 已通过，不再出现 `PageInvalidFetch bad addr = 0x0`。
- la64 musl/glibc focused `pthread_cancel_points` 不再触发内核异常，剩余失败变为用户态断言，例如 `shm_open` cancellation point 行为、non-blocking `pthread_join` 取消语义。
- glibc dynamic `pthread_cancel*` 仍受测试环境缺 `libgcc_s.so.1` 影响，这不是本次 signal-frame 控制流问题。

更新结论：la64 `pthread_cancel*` 的 P0 级“信号返回后跳空地址”已消除。后续继续处理时，应把 `pthread_cancel_points` 拆成 cancellation-point 语义、`pthread_join` 非阻塞语义和 glibc runtime 依赖三类，而不再按 `pc/ra` 恢复损坏处理。

## 7. 各测试组问题记录

| 组 | la64 | rv64 | 主要问题 |
| --- | --- | --- | --- |
| basic | PASS | PASS | 基础 syscall 路径可用 |
| busybox | FAIL | FAIL | musl/glibc 子脚本 exit_code 均为 0，但 `hwclock` 打印 `/dev/misc/rtc: No such file or directory`，触发 wrapper 的硬失败规则 |
| lua | PASS | PASS | 未见明显问题 |
| libctest | FAIL | PASS | 2026-05-20 la64 pthread/TLS 大量用户态异常并最终超时；2026-05-21 clone ABI 修复后 TLS/cond/once 类问题明显收敛，signal-frame sigmask 修复后 `pthread_cancel`/`pthread_cancel_sem_wait` 不再 0 地址取指，剩余集中在 `pthread_cancel_points` 语义、syscall 435 和 libc 细节 |
| iozone | PASS | PASS | 双架构通过；本轮未见 heap/OOM/ext4 panic |
| unixbench | FAIL | TIMEOUT | 2026-05-20 la64 直接 `Fork failed ... Bad address`；2026-05-21 复测已跑过 SPAWN/EXECL，未再见 Bad address，剩余为 timeout/性能问题 |
| iperf | TIMEOUT | TIMEOUT | la64 卡在 `BASIC_UDP begin`；rv64 UDP `Connection refused` 后 TCP 被外层超时打断 |
| libcbench | PASS | FAIL | la64 wrapper PASS 但 `b_regex_search` 附近有 `PageInvalidStore`；rv64 glibc pthread benchmark 受 unsupported syscall 435 和 StorePageFault 影响，3 次 60s 超时 |
| lmbench | FAIL | TIMEOUT | 双架构都能打印部分 latency 数字，但 musl/glibc 多轮 60s 超时；更偏性能/阻塞路径问题 |
| netperf | FAIL | TIMEOUT | 双架构 UDP_STREAM 都卡；musl `setsockopt errno 92`，glibc `getprotobyname`/无响应；内部 90s 重试耗尽或外层 600s 超时 |
| cyclictest | PASS | PASS | wrapper PASS，但双架构内部 `NO_STRESS_P1/P8`、`STRESS_P1/P8` 都是 `end: fail`；依赖 unsupported syscall 236/120/121。2026-05-21 la64 hackbench 已从 Bad address 变为 Resource temporarily unavailable |
| ltp | TIMEOUT | TIMEOUT | 两边都推进到 cgroup 段后外层 600s 超时；每边记录到 212 个 `FAIL LTP CASE` |

## 8. LTP 地基问题

### 当前 LTP 失败分布

| 架构 | `FAIL LTP CASE` 数量 | exit code 分布 |
| --- | ---: | --- |
| la64 | 212 | 32:101, 2:79, 0:20, 6:4, 36:3, 128:2, 3:2, 1:1 |
| rv64 | 212 | 32:124, 2:42, 0:32, 6:6, 1:2, 128:2, 3:2, 36:2 |

这些数字不能直接等同于真实内核 bug 数量，因为大量是 `TCONF`、缺测试依赖、缺模块或缺 procfs/sysfs 文件。后续 LTP 统计必须按 `TPASS/TFAIL/TBROK/TCONF` 解析，而不是只看 `FAIL LTP CASE` 标签。

### procfs/sysfs 缺口

本轮未再观察到 `/proc/<pid>/stat` 缺失；剩余高频缺口如下：

- `/proc/self/maps`：`accept03` 等用例需要。
- `/proc/sys/kernel/pid_max`：`capget02` 等用例需要。
- `/proc/sys/kernel/tainted`：多个 bpf 用例需要。
- `/proc/self/mounts`：cgroup 用例直接 TBROK。
- `/proc/sys/user/max_user_namespaces`：命名空间相关用例探测。

这些适合先做最小兼容文件，不需要一开始实现完整 Linux 语义。目标是让 LTP 能越过环境探测阶段，暴露真正的 syscall 行为问题。

### 需要优先跳过或分类的 LTP 子域

- cgroup：当前无 cgroup 实现，且 `cgroup_fj_proc` 会吃掉外层 600s。
- capabilities：`capget/capset` 未实现，POSIX capability 缺失。
- module/vcan/AF_ALG/libaio/keyctl/bpf：大量 `TCONF` 或依赖 Linux 模块。
- IPv6/raw protocol：`asapi_01`、broken_ip、busy_poll 等用例需要更完整的网络协议/环境。

建议先建立 LTP include/exclude 基线，把“不计划支持/环境不适用”的用例从适配对象里剥离，避免每次全量都被 cgroup/net/module 类用例消耗时间。

## 9. 网络问题

- `netperf` 双架构都卡在 UDP_STREAM：
  - musl：`enable_enobufs failed: setsockopt (errno 92)`，随后 `recv_response_timed_n: no response received`。
  - glibc：`enable_enobufs failed: getprotobyname` 或无响应。
- `iperf`：
  - la64：停在 `BASIC_UDP begin`，外层超时。
  - rv64：UDP 连接被拒绝，随后 TCP 阶段超时。

初步归类：这是 socket option 语义、协议数据库/环境文件、server/client 生命周期和阻塞网络路径的组合问题。它不是本轮 task 重构效果的核心指标，但会影响后续全量评分，建议单独建网络适配任务。

## 10. 后续优先级

### P0：先打通 LTP/task 地基

1. la64 fork EFAULT：已由 2026-05-21 `clone` ABI 修复解决主因。下一轮全量应确认该类 `Bad address` 是否归零。
2. procfs 最小兼容：补 `/proc/self/maps`、`/proc/self/mounts`、`/proc/sys/kernel/pid_max`、`/proc/sys/kernel/tainted`、`/proc/sys/user/max_user_namespaces`。
3. LTP 解析和跳过规则：按 `TPASS/TFAIL/TBROK/TCONF` 分类，默认跳过 cgroup、capability、module/vcan、AF_ALG、libaio、keyctl、明显不适配的 bpf/IPv6 模块用例。
4. hackbench/UnixBench 资源压力：针对当前 `Resource temporarily unavailable` 和 UnixBench timeout，检查 task 数量上限、fd 分配、线程回收、zombie 清理和 wait 路径。

### P1：线程和网络兼容

1. syscall 435：la64/glibc 与 rv64 线程路径均会触发，优先确认是否为 `clone3`，决定实现最小兼容还是在 libc/测试层退回 `clone`。
2. pthread_cancel_points：signal-frame sigmask 修复后，`pthread_cancel*` 的 `0x0` 取指已消除；下一步重点看 `shm_open` 等 cancellation point 是否触发取消、non-blocking `pthread_join` 语义、线程退出状态和 glibc dynamic `libgcc_s.so.1` 依赖。
3. cyclictest 依赖 syscall 236/120/121：先映射真实 syscall 名称，再决定 stub/实现/跳过。
4. netperf/iperf：先修 `setsockopt errno 92` 和 glibc `getprotobyname` 环境，再看 server 生命周期和阻塞唤醒。

### P2：benchmark 和 libc 细节

1. busybox：补 `/dev/misc/rtc` stub，或调整 wrapper 不把这个已知缺设备输出当组失败。
2. libcbench regex StorePageFault：先复现 `b_regex_search ("a{25}b")` 附近崩溃。
3. libctest libc 语义：locale/scanf/stat time/socket option 等不直接属于 task，但会持续污染通过率。
4. unixbench/lmbench：先区分真实死锁和单纯慢；必要时单独提高内部 timeout 做性能基线。

## 11. 下一轮建议验证矩阵

- 构建：仍用 Docker 串行 `make all`。
- la64 task 定向：重跑 `ltp_runner=inline` 中原先触发 fork EFAULT 的子集，并确认 `fork(): EFAULT` / `Bad address` 归零。
- clone/thread 定向：双架构跑 `libcbench` glibc pthread 片段、`libctest` 的 `pthread_cancel*` / `pthread_robust_detach` / `pthread_condattr_setclock` / `setvbuf_unget`，重点分离 clone3、pthread_cancel 和 clear_child_tid 问题。
- fork 压力定向：la64 `cyclictest` hackbench 与 `unixbench` SPAWN/EXECL，确认剩余是否都是 `EAGAIN`/timeout，而不是 EFAULT。
- procfs 定向：在 initproc 或 busybox 中直接读 `/proc/1/stat`、`/proc/self/maps`、`/proc/self/mounts`、`/proc/sys/kernel/pid_max`。
- 网络定向：只跑 netperf UDP_STREAM 和 iperf UDP/TCP，打开 socket option 与 wait_io/poll 日志。
