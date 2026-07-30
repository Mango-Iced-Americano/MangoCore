# SMP B32 `sched_getaffinity` 证据摘要

## 结论

状态：`pass`。

B32 将已有的 raw `sched_getaffinity()` stub 接到 B31 的 per-thread
`cpus_allowed`。`pid=0` 返回调用线程的 mask，正数只按 TID 查询；当前
`MAX_CPUS=8` 的 mask 固定占一个 `usize`，成功返回实际复制的 8 字节。

本节点只提供查询语义。`sched_setaffinity()`、运行期 mask 写入、强制迁移、默认
全核 affinity 和普通用户任务全核解封均未实现。

## 上游依据

Linux `sched_getaffinity` 的 raw syscall 实现先检查 mask 长度能表示 `nr_cpu_ids`，再要求
长度是 `sizeof(unsigned long)` 的整数倍；成功时复制
`min(len, cpumask_size())` 字节并返回该长度。affinity 是线程属性，`pid=0` 表示
调用线程，正数表示 TID。

参考：

- Linux `kernel/sched/syscalls.c` 中的
  [`sched_getaffinity`](https://github.com/torvalds/linux/blob/master/kernel/sched/syscalls.c)
- Linux man-pages 的
  [`sched_setaffinity(2)`](https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html)

MangoCore 双架构均为 64 位且 `MAX_CPUS=8`，因此内核 mask 大小固定为 8 字节。
实现不会清零调用者在这 8 字节之外提供的更大缓冲区，因为那部分不属于本次复制范围。

## 生产实现

`os/src/syscall/process/ids.rs::sys_sched_getaffinity()` 的顺序固定为：

1. 验证 `cpusetsize >= size_of::<usize>()` 且为该宽度的整数倍；
2. `pid=0` 克隆本 CPU current TCB，正数调用 `ProcessManager::find_task(tid)`；
3. registry 锁释放后读取目标 TCB 的原子 `cpus_allowed`；
4. 用调用者的 `current_user_token()` 写回一个 `usize`；
5. 成功返回 8，uaccess 原样传播 EFAULT，目标不存在返回 ESRCH。

这里没有使用 `find_task_for_pid_or_current()`。该 helper 为其他旧 scheduler ABI
提供 PID fallback，会模糊主线程 PID 与任意 TID；affinity 查询不应继承该行为。

用户拷贝可能 fault-in，但它发生在 registry 锁释放、mask 快照完成之后。函数不获取
task inner 或 runqueue 锁，也没有新增普通锁、等待点、`unsafe`、IPI reason 或状态机。

## 内存序边界

B31 只允许创建路径在任务仍为 `New` 时写 mask，首次发布后保持不可变。写入随后经
`New -> Queued` 的 AcqRel 调度状态交接和 runqueue 锁发布；能够执行 syscall 的任务已经
历经 `Queued -> Running`。因此本批的 Relaxed 原子读取只是稳定值快照，不需要另加
Acquire 或 task 锁。

这一证明不能外推到未来的 `sched_setaffinity()`：运行期 writer 必须把 mask 变化和
Running/Queued/Blocked 的 owner 迁移放在新的串行化协议内。

## 用户态动态验证

既有 RV64/LA64 用户 probe 使用同一 16 字节对齐栈帧：

| 区域 | 用途 |
|---|---|
| `sp + 0 .. 8` | `sched_getaffinity` 写入的 `usize` mask |
| `sp + 8 .. 12` | `getcpu` 写入的 `u32` CPU 编号 |

执行链为：

1. CPU0 获取单线程主线程的正 ID；
2. `sched_getaffinity(id, 8, sp)` 必须返回 8 且写出 `0b11`；
3. `getcpu` 必须写出 0；
4. 真实 `sched_yield` 将同一 TCB 交给 CPU1；
5. `sched_getaffinity(0, 8, sp)` 必须再次返回 8 和 `0b11`；
6. `getcpu` 必须写出 1，最后 exit(0)。

mask slot 在两次调用前清零；任一 syscall 返回值、mask、CPU 或 yield 失败都进入唯一的
exit(1) 路径。这样可以推翻旧 stub 的固定 bit0 和迁移后 current 取错等错误。

该 probe 是单线程 leader，正 ID 同时等于 PID/TID。它不能动态区分“主线程 PID 特判”与
“严格 TID 查找”；后者由生产源码中唯一的 `find_task(tid)` 路径证明。未来需要覆盖
非 leader 查询时，应增加真正的 CLONE_THREAD 用例，不能把本结果描述为已经覆盖。

## DeepSeek 只读审查

- job：`smp-b32-getaffinity-review-001`
- 耗时：257.689 秒
- exit：0
- mutation：false
- 结论：无 blocker；长度/返回值、锁释放、B31 内存序和双架构汇编链闭合。

人工没有采纳两条非阻断建议：

- 不清零 8 字节之外的用户缓冲区；Linux 只复制实际 cpumask 长度。
- 不增加“负 pid → EINVAL”分支；当前 Linux `find_process_by_pid()` 没有该显式检查，
  查找失败按 ESRCH 处理。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP |
|---|---|---:|---:|---:|---:|
| RV64 | `agent-2c5db1c3f621-r01-rv64-ktest` | 135.383s | 0 | `0xff` | 21/21 |
| LA64 | `agent-2c5db1c3f621-r02-la64-ktest` | 137.969s | 0 | `0xff` | 21/21 |

两份日志都明确包含：

```text
[smp] minimal boot ready: configured=8 ... online_mask=0xff
ok 20 smp::user_task_migrates_on_yield
[KTEST RESULT: PASS]
```

两个架构实际汇编、链接并执行了各自的 user probe，因此 `sd/ld` 与 `st.d/ld.d`、
分支标签、syscall 参数寄存器和 trap 寄存器保存不只经过静态推断。

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-2c5db1c3f621-r03-rv64-preliminary` | 360.737s | 0 | 312/314 | 两套 `busybox kill 10` 各 0/1 |
| LA64 | `agent-2c5db1c3f621-r04-la64-preliminary` | 358.815s | 0 | 308/314 | 两套 `test_brk` 各 1/3；两套 `busybox kill 10` 各 0/1 |

两架构均为 `CORE_NUM=8`、`mask=0x003`、`online_mask=0xff`，四个
basic/busybox END 与 runner done 完整。四个 child 均 exit 0、未超时、无 forbidden
marker，source-before/source-after HEAD、status 和 tracked diff 指纹一致，
`mutation_detected=false`。

## 被测源码与本地协作边界

- branch：`smp`
- HEAD：`56330f31346bf4f9f9c195f96b0a12f5918b4b63`
- 被测功能源码 diff SHA-256：
  `6c88ee35ad1daa5a5d1909ceedb85a2973ef75c0de9f84622624252704ec1a4a`
- DeepSeek 冻结验证 job：`smp-b32-getaffinity-validation-001`
- 总耗时：1165.138 秒

所有 prompt、模型输出和原始 stdout/stderr 只保存在本地忽略的 `cc-codex/`，不纳入
GitHub。测试完成后只新增本文档及同步已有文档，没有修改被测功能源码，也没有遗留
临时用户 ELF、调试字段、`.orig` 或 `.rej`。
