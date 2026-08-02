# B73 进程级 rlimit owner 证据

## 结论

状态：`pass`。

B73 将 FSIZE、STACK、CORE、NPROC、MEMLOCK、SIGPENDING、NICE 和 RTPRIO 的唯一 owner
从 TCB 迁入 PCB。线程 clone 共享同一个 owner；普通 fork 在父 owner 锁内复制完整快照后
创建独立 owner；exec 复用 PCB 并保留限制。CPU 与 NOFILE 不在本节点完成，不能据此宣称全部
rlimit 已具备完整进程语义。

## 设计依据

Linux 6.6 把 rlimit 存在进程信号共享对象的 `rlim[]` 中；`CLONE_THREAD` 的 `copy_signal()`
直接复用共享对象，普通 fork 才复制 rlimit；`do_prlimit()` 也在该共享 owner 内读写 pair：

- <https://github.com/torvalds/linux/blob/v6.6/include/linux/sched/signal.h>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/fork.c>
- <https://github.com/torvalds/linux/blob/v6.6/kernel/sys.c>

MangoCore 用 PCB 表达等价共享域。`ProcessLimits` 只保存 ABI pair；消费者在锁内复制单个 soft
limit，解锁后才获取 `task.inner`、VM、signal queue 或进入文件路径，因此没有新增嵌套锁边。

CPU 没有只做字段迁移：当前运行时间仍按 TCB 统计，必须和线程组累计、信号投递以及热路径
争用一起设计。NOFILE 仍依附 fd table；在处理 `CLONE_FILES` 跨进程共享前直接搬入 PCB 会
把 fd-table 属性错误变成进程属性。这两个边界明确留给后续独立节点。

## 冻结源码

- 基线 commit：`149fb23d90f5a5e35b9d33c3764682860c5617ec`
- 生产代码 diff SHA-256：
  `553376efdf09757a1defb704543eeb066e8a8b28fdc979d8c6cafaa4b477f167`
- `os/src/task/process.rs`：
  `8afd01b591f02c0f85cef0efb836168e02ef039e7c4257391a7e8cda497042fd`
- `os/src/task/task.rs`：
  `7f98aac719d120e04034f62ee4cb3371b23559a5337dc52b163154818af4948a`
- `os/src/syscall/process/ids.rs`：
  `0b7f52e3a8b9a7f0535ab44148df79ba1b4c52a4589c1e615e1eb2d546ecde5a`
- `git diff --check`：通过。

DeepSeek 只读设计审查和冻结 diff 复审均未修改源码；最终 P0/P1 为 0。GPT/Codex 独立复核
三个 PCB 构造点、旧 TCB 字段引用、thread/fork/exec 生命周期和所有消费者锁边。

## Docker 验证

本地任务：`smp-b73-process-rlimit-validation-r1`。四项 recipe 严格串行执行，全部
`CORE_NUM=8`，测试前后生产代码 diff 指纹一致。

| Recipe | 结果 | 耗时 |
|---|---:|---:|
| `rv64-kernel-build` | PASS | 130.581s |
| `la64-kernel-build` | PASS | 137.569s |
| `rv64-prlimit-gate` | PASS | 155.097s |
| `la64-prlimit-gate` | PASS | 157.570s |

两架构 QEMU 均打印 `configured=8`、`online_mask=0xff`。focused gate 明确使用
`exclude=none`，musl 与 glibc 各运行以下 9 项，全部 PASS、0 fail、0 skip：

```text
getrlimit01 getrlimit02 getrlimit03
setrlimit01 setrlimit02 setrlimit03 setrlimit04 setrlimit05 setrlimit06
```

原始日志独立检查未发现 panic、超时或缺失的结束标记。`setrlimit04` 覆盖普通 fork 继承；
`setrlimit06` 证明本批没有破坏仍位于 TCB 的既有 CPU-limit 路径。

## 证据边界

- 多线程中“一线程 set、另一线程立即 get”的精确交错：`not-run`。共享结论来自同一 PCB Arc
  与唯一 mutex owner 的源码证明，普通 LTP 不能冒充该动态覆盖。
- RLIMIT_CPU 线程组时间核算：`not-run / not-implemented`。
- RLIMIT_NOFILE 与 `CLONE_FILES` 跨进程分离：`not-run / not-implemented`。
- FSIZE 的六个窄消费者做了必要 owner 读取迁移，但 FS/Net/Driver 完整 SMP 审计仍由对应负责
  人推进，本节点不作全子系统安全声明。
