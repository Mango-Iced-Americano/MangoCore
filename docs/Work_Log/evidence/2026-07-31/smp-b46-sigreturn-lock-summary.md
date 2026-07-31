# SMP B46 sigreturn 锁外读取实施证据

状态：`pass`

## 变更边界

- `sys_sigreturn()` 只在开始快照用户 SP、结束提交恢复态时短持 `task.inner`；用户
  sigmask、machine context 和 LA64 LSX 的读取全部位于锁外。
- 双架构 `TrapContext` 以 `machine_context()`/`set_machine_context()` 显式复制
  `gp/fp`，删除 `TrapContext * -> MachineContext *` 的前缀布局强转。
- signal frame 任一字段读取失败时不提交任何恢复态；退出 helper 在 noreturn 调度前
  释放本层 task `Arc`。
- 本节点没有重写 `do_signal()`、pending 队列、signal disposition 或退出状态机，
  也没有新增文件、调度状态或测试专用生产字段。

## 正确性边界

当前线程执行 `rt_sigreturn` 时仍由本 CPU current 槽唯一拥有。远端普通信号只把条目
追加到 pending 队列；exec、group-exit 和 affinity 请求由 owner 在返回安全点消费，
都不越过 `task.inner` 直接改写 live trap frame。因此可以在第一次解锁后读取用户
frame，再以一次短临界区提交，而不需要新增 trap generation。

用户读取可能触发缺页、CoW 和 TLB shootdown，不能跨它持有普通 task 锁。先完整读取
到局部值再提交也保证畸形 frame 不会产生“sigmask 已恢复但寄存器未恢复”之类的部分
状态。LA64 先安装完整 LSX，再以 machine context 中标量 FPR 覆盖每个向量低 64-bit
lane，保留既有 ABI 优先级。

## 官方资料与 DeepSeek 裁决

- Linux v6.6 RISC-V `restore_sigcontext()` 从用户 signal frame 读取寄存器和浮点状态，
  再恢复当前线程架构状态。
- Linux v6.6 LoongArch `restore_sigcontext()` 同样围绕 user copy 和架构扩展恢复组织；
  MangoCore 只借鉴该边界，不照搬 Linux 的 thread state 或 ABI 结构。
- DeepSeek 冻结只读设计审查结论为 `ACCEPT`：确认 current owner 不变量、
  read-all-then-commit、错误路径 Arc 释放，以及 LA64 “LSX 后由标量低 lane 覆盖”的
  顺序。审查建议把 `do_signal()` 的用户栈写入也移到锁外，该项作为 B47 单独设计，
  未扩张 B46。

## 环境与源码指纹

- 被测 HEAD：`12b54ce0379afb29e4358f1d5ec4bf2b265302cc`
- 冻结可执行源码 diff SHA-256：
  `55a3604be14f55b7ab5ac65d40e76c1eab2d26d26dcd5bf2d7693ba37a75d772`
- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2

## 验证结果

DeepSeek 通过只允许本任务两项 recipe 的受限网关，在同一 Docker 容器内串行执行：

| Job | 配置 | 结果 | 用时 |
|-----|------|------|------|
| `agent-ddeeff60f5b6-r01-rv64-preliminary` | RV64，8 核，`mask=0x003` | 312/314 | 344.013 s |
| `agent-ddeeff60f5b6-r02-la64-preliminary` | LA64，8 核，`mask=0x003` | 308/314 | 346.631 s |

RV64 仅 busybox-musl/glibc 的 `busybox kill 10` 各失败一次。LA64 仅
basic-musl/glibc 的 `test_brk` 各 1/3，以及两套 `busybox kill 10` 失败。两者与
B45 基线完全一致；clone/fork/exec/exit/wait 等相关项均保持满分。

两个 child 均 `process_exit_code=0`、`timed_out=false`、无 forbidden marker，
且 before/after 的 HEAD、status SHA-256 和 tracked diff SHA-256 完全一致，
`mutation_detected=false`。Codex 另行读取原始 stdout 中的 judge JSON，独立确认
总数与失败身份。

## 未覆盖项

正常初赛流程通过 PID1 的 SIGCHLD handler 反复触发真实 signal frame 往返，但没有
刻意提交溢出地址、不可读 sigmask、损坏 machine context 或损坏 LSX。因此本节点动态
验收的是正常恢复主路径和双架构非回归；畸形 frame 的各个 SIGSEGV 分支仍以源码路径
审查为证据，不能描述为已被逐项运行。
