# SMP B47 signal frame 锁外写入实施证据

状态：`pass`

## 变更边界

- `do_signal()` 只在 pending/action 选择、返回态快照和 handler 上下文提交时短持
  `task.inner`；完整 `SigInfo + UserContext` 在 `task.inner`、`sighand` 都释放后写入。
- `SA_RESETHAND` 在 sighand 锁内复位；完整用户 frame 写成功后才发布 handler
  PC/SP/参数寄存器和 mask。
- 双架构不再区分 `SA_SIGINFO`/非 `SA_SIGINFO` 的 frame 布局；删除仅服务于手工
  分字段写入的 `UserContext::encode_sigmask()`。
- 本节点没有修改 pending/default action/退出状态机，没有新增文件、generation、
  调度状态或测试专用生产字段。

## 正确性边界

当前线程在 trap return 前仍由本 CPU current 槽唯一拥有，只有 owner 会改写 live trap
frame。远端普通 signal 只追加 pending；exec、group-exit 和 affinity 请求都在 owner
安全点生效。因此用户栈写入期间可以释放 task 锁，不需要新增 trap generation。

用户写入可能缺页、CoW 或等待 TLB shootdown，不能跨它持有普通任务锁。先写完整 frame、
再发布 handler 上下文，保证 CPU 不会执行只拥有半个 frame 的 handler。写入失败时，
live trap context 和 signal mask 尚未切换到 handler，当前任务按 `SIGSEGV` 退出。

`SA_RESETHAND` 必须在复制 action 后、释放 sighand 锁前复位，否则同进程另一个线程可
再次观察到旧的一次性 handler。该 disposition 更新先于可能失败的 frame 写入，与
Linux 的 action 线性化顺序一致。

## 官方资料与 DeepSeek 裁决

- Linux v6.6 `get_signal()` 在 sighand 锁保护下复制 action，并在返回调用者前处理
  `SA_RESETHAND`。
- Linux v6.6 RV64/LA64 rt signal 路径都先写完整 frame，再设置 handler PC、SP 和
  `a0/a1/a2`；两架构都提供 signal、siginfo、ucontext 三个参数寄存器。
- DeepSeek 冻结设计审查结论为 `ACCEPT`，确认 lock-free uaccess、写完再发布、
  `SA_RESETHAND` 线性化和 current owner 证明。
- DeepSeek 最终总结遗漏 LA64 两个 `test_brk` partial failure，并误称初赛已动态覆盖
  `SA_SIGINFO`、altstack、`SA_NODEFER` 和 restart 组合。Codex 读取原始 judge JSON
  与 PID1 action 源码后纠正：动态证据直接覆盖的是非 `SA_SIGINFO` 正常 SIGCHLD frame
  往返；其他 flag/错误分支仍是源码审查边界。

官方源：

- <https://raw.githubusercontent.com/torvalds/linux/v6.6/kernel/signal.c>
- <https://raw.githubusercontent.com/torvalds/linux/v6.6/arch/riscv/kernel/signal.c>
- <https://raw.githubusercontent.com/torvalds/linux/v6.6/arch/loongarch/kernel/signal.c>

## 环境与源码指纹

- 被测 HEAD：`95538a23f0c0956dfe3a9b518dfb4ba0d06e8d5a`
- 冻结可执行源码 diff SHA-256：
  `0cda317e4a5f7ed640136135e57634c9cc16555a0d5aa3fc3da86e6ed5b255bb`
- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2

## 验证结果

DeepSeek 通过受限网关在 Docker 内严格串行执行：

| Job | 配置 | 结果 | 用时 |
|-----|------|------|------|
| `agent-2541d474c27f-r01-rv64-preliminary` | RV64，8 核，`mask=0x003` | 312/314 | 344.371 s |
| `agent-2541d474c27f-r02-la64-preliminary` | LA64，8 核，`mask=0x003` | 308/314 | 363.968 s |

RV64 仅 busybox-musl/glibc 的 `busybox kill 10` 各失败一次。LA64 仅
basic-musl/glibc 的 `test_brk` 各 1/3，以及两套 `busybox kill 10` 失败。两者与
B46 完全一致。

两个 child 均 `process_exit_code=0`、`timed_out=false`、无 forbidden/missing marker，
且 before/after 的 HEAD、status SHA-256 和 tracked diff SHA-256 完全一致，
`mutation_detected=false`。Codex 独立读取原始 stdout 的 judge JSON，确认总数与
失败身份。

## 未覆盖项

normal PID1 在 `user/src/bin/init.rs` 安装带 `SA_RESTART`、不带 `SA_SIGINFO` 的
SIGCHLD handler，初赛会反复经过新的完整 frame 投递与 `rt_sigreturn`。该事实不能
外推为 `SA_SIGINFO`、`SA_ONSTACK`、`SA_NODEFER`、`SA_RESETHAND` 或 syscall restart
分支已分别动态触发，也没有刻意制造 frame 写失败。本节点对这些分支只提供源码审查
证据。
