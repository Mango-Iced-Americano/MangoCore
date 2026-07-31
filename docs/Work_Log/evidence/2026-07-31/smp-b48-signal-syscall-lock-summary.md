# SMP B48 signal 状态 syscall 锁边界证据

## 1. 结论

状态：`pass`

B48 把 `sigaction()`、`sigprocmask()` 和 `sigaltstack()` 中可能缺页的用户访问移出
`sighand`/`task.inner`，同时保持共享 action 或线程本地 signal 状态在单个短临界区内
快照并提交。实现没有引入新 helper、状态对象、文件或测试专用生产字段。

冻结源码在双架构 8 核 `mask=0x003` 门禁中没有退化：

| 架构 | online mask | 得分 | 精确失败集合 | 结果 |
|------|-------------|------|--------------|------|
| RV64 | `0xff` | 312/314 | musl/glibc `busybox kill 10` 各 0/1 | pass |
| LA64 | `0xff` | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 | pass |

## 2. 冻结源码

- 分支：`smp`
- 被测 HEAD：`0fd90ac122a748ce75d27c7ba5640e99269f9332`
- 可执行源码：
  - `os/src/task/signal/mod.rs`
  - `os/src/syscall/process/signal.rs`
- 源码改动：101 行增加、119 行删除
- tracked diff SHA-256：
  `33a8f1ccbf41278a8132b928542d928ca0c485ce3ee7f6fa6bd079ee971f7644`
- 两个测试 child 的 HEAD、status、tracked diff 和 untracked-content 指纹在运行前后
  完全一致，`mutation_detected=false`。

## 3. 实现不变量

### 3.1 `sigaction`

```text
锁外读取 act
  -> sighand：快照 old action + 提交 new action
  -> 解锁
  -> 锁外写 oldact
```

- 旧 action 快照和新 action 替换共用一个线性化点。
- 输入 `EFAULT` 时尚未修改 disposition。
- oldact copyout `EFAULT` 时新 action 已经提交，不回滚。
- `act == oldact` 时先完成输入读取，避免 copyout 覆盖尚未读取的新值。

### 3.2 `sigprocmask`

```text
锁外读取低 64 位 set
  -> task.inner：快照 old mask + 应用 how
  -> 解锁
  -> 锁外写低 64 位 oldset
```

- 用户 ABI 固定为 `u64`；内部 `Signals` 的架构相关宽度不进入 uaccess 类型。
- `set == NULL` 时只查询并忽略 `how`。
- 新 mask 始终移除 `Signals::CAN_NOT_BE_MASKED`。

### 3.3 `sigaltstack`

```text
锁外读取 ss
  -> task.inner：快照当前 SP/old stack + 校验并提交 new stack
  -> 解锁
  -> 锁外写 old_ss
```

- ONSTACK、DISABLE、AUTODISARM、最小大小和地址溢出检查仍在提交前完成。
- 输入或校验失败时不提交；成功提交后的 copyout `EFAULT` 不回滚。

## 4. 官方语义裁决

核对对象：

- Linux v6.6 `kernel/signal.c`
  - `SYSCALL_DEFINE4(rt_sigprocmask)`：内部 `sigprocmask()` 返回错误后立即返回，只有成功
    才执行 `copy_to_user(oset, ...)`。
  - `SYSCALL_DEFINE2(sigaltstack)`：只有 `!err` 才执行
    `copy_to_user(uoss, ...)`。
  - `SYSCALL_DEFINE4(rt_sigaction)`：先 copyin 新 action，`do_sigaction()` 成功后才
    copyout 旧 action。
  - `do_sigaction()` 允许 `act == NULL` 时查询 `SIGKILL/SIGSTOP`，但仍拒绝
    `sig < 1`；MangoCore 对不可更改信号的纯查询限制是既有差异，不在 B48 扩展范围。

官方源码：
<https://raw.githubusercontent.com/torvalds/linux/v6.6/kernel/signal.c>

## 5. DeepSeek 只读审查与裁决

### 5.1 独立审查

- Job：`smp-b48-signal-syscall-review`
- 进程退出码：0
- 用时：484.582 秒
- 源码 mutation：false
- 初始结论：`ACCEPT`，无 mandatory finding。

该报告正确确认三个目标函数的所有 `UserPtr`/`UserPtrMut` 都位于普通锁外，并确认
共享 action 的快照与替换在一个 `sighand` 临界区内。报告关于
`rt_sigprocmask`/`sigaltstack` 错误路径仍 copyout 旧值的两点解释不符合 Linux syscall
wrapper，未采纳。

### 5.2 双架构验证汇总

- Job：`smp-b48-signal-syscall-validation`
- 模式：自然语言派活、只读源码、effort max
- 进程退出码：0
- 总用时：883.439 秒
- 执行顺序：RV64 完成后才启动 LA64
- 源码 mutation：false

验证汇总把同一 `sigaltstack` 误读升级为 mandatory，并误称 `sigaction(0)` 是合法
查询。Codex 以 Linux v6.6 `sigaltstack` wrapper 的 `if (!err && uoss)` 和
`do_sigaction()` 的 `sig < 1` 检查否决，没有修改冻结源码。

## 6. Docker 与 QEMU 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- image tag：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- image created：`2026-05-10T08:46:16.065707166Z`
- RV64 QEMU：10.0.2
- LA64 QEMU：10.0.2

## 7. 验证明细

### RV64

- Child job：`agent-c65534449026-r01-rv64-preliminary`
- Recipe：`rv64-preliminary`
- 参数：`CORE_NUM=8`、`mask=0x003`
- normal kernel build：包含在 recipe 中，成功
- QEMU 退出码：0
- 用时：351.488 秒
- `online_mask=0xff`
- judge：312/314
- 失败：
  - busybox-musl `busybox kill 10`：0/1
  - busybox-glibc `busybox kill 10`：0/1
- timeout：false
- forbidden marker：无
- mutation：false

### LA64

- Child job：`agent-c65534449026-r02-la64-preliminary`
- Recipe：`la64-preliminary`
- 参数：`CORE_NUM=8`、`mask=0x003`
- normal kernel build：包含在 recipe 中，成功
- QEMU 退出码：0
- 用时：359.102 秒
- `online_mask=0xff`
- judge：308/314
- 失败：
  - basic-musl `test_brk`：1/3
  - basic-glibc `test_brk`：1/3
  - busybox-musl `busybox kill 10`：0/1
  - busybox-glibc `busybox kill 10`：0/1
- timeout：false
- forbidden marker：无
- mutation：false

两架构的精确失败集合均与 B47 人工接受基线一致。

## 8. 证据边界

- preliminary 只运行 basic + busybox，不等于 LTP/libctest 信号全量验证。
- normal 用户路径能证明常用 signal 初始化没有退化，但没有专门构造：
  - 输入/输出指针别名；
  - copyin/copyout `EFAULT`；
  - 非法 `how` 或 altstack 参数；
  - 当前 SP 位于 altstack；
  - 多线程同时替换同一 signum。
- B48 提交后按维护者要求暂停；本证据不宣称下一工作包已经开始。
