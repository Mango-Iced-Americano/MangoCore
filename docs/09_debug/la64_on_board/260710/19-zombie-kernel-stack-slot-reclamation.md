---
title: "2K1000LA zombie TCB 滞留与 1024 个内核栈 slot 耗尽复盘"
category: debug
status: resolved-with-known-limits
author: MangoCore Team
last_update: 2026-08-01
tags: [postmortem, la64, 2k1000la, task, zombie, kernel-stack, resource-leak, ltp]
code_paths:
  - "os/src/task/manager.rs"
  - "os/src/task/process_manager.rs"
  - "os/src/task/task.rs"
  - "os/src/task/quota.rs"
  - "os/src/hal/arch/loongarch64/config.rs"
  - "os/src/hal/arch/loongarch64/kern_stack.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/02-valen40-kernel-stack-and-tlb.md"
  - "docs/05_process/exit-wait.md"
  - "docs/05_process/task-control-block.md"
evidence_commits:
  - "ebd27f76"
  - "1ace76e5"
evidence_records:
  - "docs/Work_Log.md, 2026-06-09 la64 full-suite stack-slot panic"
  - "docs/Work_Log.md, 2026-07-12 board core-test validation"
  - "logs/board-perf-ahci-20260714/baseline-85314659.log"
---

# 2K1000LA zombie TCB 滞留与 1024 个内核栈 slot 耗尽复盘

> **当前实现注记（2026-08-01）：** 本文为历史故障复盘，下文的全局
> `zombie_queue` 代码和三队列模型对应当时的修复。SMP B50 已将这个强 Arc
> 容器改为 Per-CPU `local_zombies`；按 pid 回收依次扫描全部 CPU 队列，
> 仍遵循“锁内摘取、锁外析构”。这不改变本文对“PCB zombie 语义”与
> “TCB/内核栈对象寿命”必须分层的根因结论。

## 0. 一句话结论

父进程 `wait` 回收子进程时已经释放 PID 和 clone quota，却只从 `ready_queue`、
`interruptible_queue` 清理该 PID 的 zombie TCB，漏掉了真正保存退出任务的专用
`zombie_queue`。队列中的强 `Arc<TaskControlBlock>` 继续持有 `KernelStack`，所以
“进程配额已归还”并不等于“LA64 guarded kernel-stack slot 已归还”。高强度
`futex_cmp_requeue01` 反复创建、退出和回收 waiter 后，分配器最终申请 slot 1024，
命中 `kstack_id >= 1024` 的确定性 panic。

提交 `1ace76e5` 将指定 PID 的清理扩展到专用 zombie 队列，并继续在锁外 drop TCB。
实板随后完成两轮含 1000 waiter 的聚焦压力而未再触发该泄漏路径。不过当前内核栈
分配仍是 infallible，1024 也是硬上限；因此本案结论是“已关闭一个可证实的 slot
滞留路径”，不是“任何 1000 线程压力都不可能再触及容量边界”。

---

## 1. 问题卡

| 属性 | 结论 |
|------|------|
| 首次明确症状 | 2026-06-09，LA64 full suite 跑到 `futex_cmp_requeue01` 后 panic |
| 直接报错 | `la64 kernel stack slot 1024 exceeds max 1024` |
| 前置信号 | `[task_quota] SOFT LIMIT reached: used=921/1024` |
| 触发负载 | 100/1000 waiter 的 futex requeue、退出与 wait 回收 |
| 已证实缺陷 | reap 已释放 quota，但 `remove_zombie_tasks_by_pid()` 未扫描 `zombie_queue` |
| 资源泄漏对象 | `zombie_queue` 的强 TCB Arc，以及 TCB 拥有的 `KernelStack` |
| 修复提交 | `1ace76e5`，`feat(board): advance 2K1000 full-system bring-up` |
| 修复后实板门禁 | 双 libc 各 274 个非网络 LTP；两次 1000-waiter 压力通过 |
| 当前残余风险 | `kstack_alloc()` 仍会在硬上限处 panic，不会返回 `EAGAIN/ENOMEM` |
| 结案状态 | 泄漏路径 resolved；容量与 fallible allocation 仍是 known limit |

这里要区分三个不同数字：

1. **clone quota 计数**：限制继续创建任务；
2. **活着或尚被强引用的 TCB 数**：决定对象是否能析构；
3. **已占用的 kernel-stack slot 数**：只有 `KernelStack::drop()` 才能回收。

本案的关键就是这三个数字发生了分离。

## 2. 现场：不是随机栈溢出，而是分配编号越界

2026-06-09 的工作日志记录了完整顺序：

```text
futex_cmp_requeue01 ... 1000 waiters
[task_quota] SOFT LIMIT reached: used=921/1024
la64 kernel stack slot 1024 exceeds max 1024
```

后续保留的实板聚合日志
`logs/board-perf-ahci-20260714/baseline-85314659.log` 也包含同型现场：

```text
futex_cmp_requeue01.c: Test 4: waiters: 100, wakes: 0, requeues: 70
TPASS: futex_cmp_requeue()
[task_quota] SOFT LIMIT reached: used=921/1024
[kernel] panicked at 'la64 kernel stack slot 1024 exceeds max 1024'
heap: 141524864/201326528 bytes free
physical frames: 49122 free
```

这组信息先排除了两个很容易误判的方向：

- panic 文本来自**新栈 slot 的边界检查**，不是 guard page 被写穿；
- 当时仍有约 141 MiB heap 和 49,122 个物理页，不能解释为普通内存耗尽。

`futex_cmp_requeue()` 的前几档已经报告 `TPASS`。也就是说，futex 唤醒/迁移语义和
“任务生命周期清理不完整”可以同时存在：用例功能断言通过，不代表每个退出 TCB 的
最后一个强引用已经消失。

## 3. 底层原理

### 3.1 LA64 内核栈不是任意地址分配，而是固定 guarded slot

当前 LA64 配置为：

```text
KERNEL_STACK_SIZE      = 0x20 * 4 KiB = 128 KiB
guard size             = 1 page      =   4 KiB
KERNEL_STACK_SLOT_SIZE = 132 KiB
KERNEL_STACK_MAX_SLOTS = 1024
```

`kernel_stack_position(kstack_id)` 先检查：

```rust
if kstack_id >= KERNEL_STACK_MAX_SLOTS {
    panic!("la64 kernel stack slot {} exceeds max {}", ...);
}
```

然后按 `KERNEL_STACK_TOP - id * SLOT_SIZE` 计算该栈的固定虚拟地址。每个 slot 中
128 KiB 映射为读写栈，剩余一页不映射作为 guard。这个设计能把跨栈越界变成确定性
异常，但也把同时未回收的 stack identity 数量限制为 1024。

### 3.2 slot 回收发生在 `KernelStack::drop()`，不发生在 `wait` 返回本身

`TaskControlBlock` 拥有一个 `KernelStack`。只有最后一个 TCB 强引用消失，Rust 才会
运行 TCB 析构，继而运行 `KernelStack::drop()`：

- 栈缓存少于 128 项时，把 slot id 放进 `KSTACK_CACHE`；
- 缓存已满时，解除内核映射，并把 id 归还 `RecycleAllocator`。

无论走哪一条，前提都是 **TCB 已经 drop**。释放 PID、把 PCB 从 children 移除、
归还 process quota，都不会自动削减其他容器中的 TCB `Arc`。

### 3.3 zombie 有两层：进程语义和调度器对象寿命

Unix 进程退出后，PCB 要保留退出码和 rusage，直到父进程 `wait`。这是进程级 zombie
语义。MangoCore 还需要让已经不能再运行的 TCB 离开调度器，因此有三类可能持有它的
队列：

```text
ready_queue
interruptible_queue
zombie_queue        <- 专门等待调度器锁外析构的强 Arc 队列
```

专用 `zombie_queue` 的存在是合理的：TCB 析构可能继续释放页表、内核栈和其他资源，
不能在持有 `TASK_MANAGER` 锁时执行整条析构链。正确协议应是：

```text
锁内：从所有队列摘下匹配 TCB，收集 Arc
解锁：drop 收集到的 Arc
最后一个 Arc 消失：TCB -> KernelStack::drop -> slot 可复用
```

问题不是使用 zombie 队列，而是指定 PID 的同步 reap 路径只清了其中两类队列。

### 3.4 为什么 soft quota 没有挡住 slot 1024

父进程成功 reap 子进程时，`ProcessManager::wait_child()` 的顺序包括：

```text
release_pid()
unregister_process()
release_process_quota_once()
remove_zombie_tasks_by_pid(pid)
```

修复前最后一步只扫描 `ready_queue` 和 `interruptible_queue`。如果 TCB 已经进入
`zombie_queue`：

1. quota 被减掉，后续 clone 又获得许可；
2. zombie TCB 仍被队列强引用；
3. 旧 TCB 的 kernel stack id 没有回收；
4. 新 TCB 获得新的、不断增大的 stack id；
5. 计数看起来回落，slot 高水位却单调逼近 1024。

所以 `used=921/1024` 只是 clone quota 的软阈值告警，不是“当前最多只占 921 个栈”
的证明。

## 4. 调试追溯

### 4.1 第一阶段：根据 panic 字符串先定资源类型

`ebd27f76` 把 LA64 kernel stack 扩到 128 KiB，解决了 `clone09` 深调用路径对 64 KiB
栈敏感的问题。随后 full suite 的新 panic 不再是随机 BTreeMap/heap 损坏，而是明确的
slot 编号越界。

这一步很重要：

- “128 KiB 仍不够大”应表现为 guard/page fault 或栈内数据破坏；
- “slot 1024 exceeds max 1024”发生在新栈地址计算前，表示身份数量耗尽。

因此继续扩大单栈大小不仅无效，还会扩大每个 slot 和物理页成本。

### 4.2 第二阶段：把 1000 waiter 看作放大器，不直接当根因

最初工作日志将现场描述为 `futex_cmp_requeue01` 留下大量 waiter。该描述准确指出了
触发负载，但还不能说明 waiter 为什么在父进程 wait 后继续占资源。

进一步审计生命周期时，关键问题变成：

> `wait_child()` 已经归还 quota 后，谁还持有这个 pid 的 TCB 强引用？

沿着 `remove_zombie_tasks_by_pid()` 检查三个队列，修复前源码只出现：

```text
ready_queue.retain(...)
interruptible_queue.retain(...)
recompute_ready_nice_count()
```

中间没有 `zombie_queue.retain(...)`。这是从症状到可执行机制的第一条直接源码证据。

### 4.3 第三阶段：用所有权链证明它确实会占住 stack

缺一个 `retain` 只有在下列链条同时成立时才会造成 slot 泄漏：

```text
zombie_queue
  -> Arc<TaskControlBlock>
  -> TaskControlBlock.kstack: KernelStack
  -> KernelStack::drop 尚未执行
  -> slot id 未进入 cache / allocator free list
```

当前源码逐项满足这条链。这里不需要猜测“也许页表没释放”；强 Arc 本身已经足以阻止
整个 TCB 的 Drop。

### 4.4 第四阶段：修复后用同型压力，而不是只跑启动 smoke

`1ace76e5` 后的实板 core-test 不是只启动到 shell，而是：

- musl、glibc 各跑 274 个非网络 LTP；
- 两轮进入 `futex_cmp_requeue01` 的 1000 waiter 档；
- 两轮均执行到组尾，未再触发 stack-slot panic；
- 总计 `passed=3569 failed=23 broken=18 skipped=94`，剩余子项问题另行统计。

这证明修复覆盖了原触发形态。它不把剩余 LTP 失败伪装成“全套 PASS”。

## 5. 根因证据矩阵

| 证据 | 能证明什么 | 不能证明什么 |
|------|------------|--------------|
| panic 精确写出 slot 1024 | 分配身份到达硬上限 | 是哪条引用链造成高水位 |
| heap/physical frames 仍充足 | 不是普通内存不足 | 不代表虚拟 slot 仍有空间 |
| quota 先报 921/1024 | 高并发任务是触发条件 | quota 数等于已占 stack 数 |
| 修复前函数不扫描 `zombie_queue` | 存在确定的 TCB 滞留路径 | 每一次 panic 都只由该路径造成 |
| `zombie_queue` 保存强 TCB Arc | 队列项阻止 TCB/stack Drop | 是否还有别的 Arc 同时滞留 |
| `1ace76e5` 加入 retain + 计数扣减 | 缺失清理已被直接补齐 | infallible 分配问题已解决 |
| 修复后两轮 1000 waiter 通过 | 原型压力下泄漏路径不再稳定触发 | 所有调度交错、所有长期负载都安全 |

因此最窄且证据充分的根因表述是：

> 指定 PID reap 路径与调度器队列集合不对称，使专用 zombie 队列中的强 TCB 引用在
> quota 释放后仍可存活，并继续占用 guarded kernel-stack slot。

## 6. 修复

提交 `1ace76e5` 在原有两类队列清理后加入：

```rust
let old_zombie_len = self.zombie_queue.len();
self.zombie_queue.retain(|task| {
    if task.process.pid == pid {
        zombies.push(task.clone());
        false
    } else {
        true
    }
});
sub_zombie_queue_count(old_zombie_len - self.zombie_queue.len());
```

外层接口仍保持：

```rust
let zombies = TASK_MANAGER.lock().remove_zombie_tasks_by_pid(pid);
drop(zombies);
```

这个细节不可省略。若直接在 `retain` 或 `TASK_MANAGER` 锁内触发最后一个 Arc drop，
析构链可能再次碰任务管理、内存映射或其他锁，形成新的锁序问题。修复同时满足：

1. 三类队列的指定 PID 清理对称；
2. `ZOMBIE_QUEUE_COUNT` 与真实队列长度同步；
3. TCB 析构继续发生在 manager 锁外；
4. 不通过扩大 1024 上限掩盖泄漏。

## 7. 验证与边界

### 7.1 已完成验证

| 环境 | 结果 |
|------|------|
| Docker rv64 kernel build | 通过，仅既有 warning |
| Docker la64 kernel build | 通过，仅既有 warning |
| 2K1000LA 聚焦镜像构建/uImage/TFTP/iminfo | 通过 |
| 实板 musl libctest | 完整结束，组退出 0 |
| 实板 glibc libctest | 完整结束；内部 19 项语义差异单列 |
| 实板 cyclictest | 双 libc 完成，无 panic |
| 实板非网络 LTP | 双 libc 各 274 项运行到组尾 |
| `futex_cmp_requeue01` 1000 waiter | 两次通过，未再触发该 panic |
| RV64 QEMU runtime | 当轮缺 `disk.img`，未进入 QEMU；只有编译通过 |

### 7.2 为什么状态不是“无条件 resolved”

当前 `kstack_alloc()` 仍调用 `kernel_stack_position()`，而后者在 id 达 1024 时 panic。
尚未完成：

- fallible stack allocation；
- clone 前把“真实已占 stack slot”纳入硬门禁；
- 为系统任务预留明确 headroom；
- 对 slot live/high-water/cache 三个数字提供持续诊断。

此外，`logs/board-perf-ahci-20260714/baseline-85314659.log` 是多次实板启动拼接的聚合
记录：其中既有多轮 1000-waiter 通过，也有一次 slot 1024 panic，但每段启动日志没有
嵌入可核验的 Git commit/hash。它能证明 1024 硬边界在该开发期确实可达，不能单凭
文件名把那一次 panic 唯一归因于“修复后相同二进制回归”。因此本文既不隐藏该记录，
也不把它过度解释为 `1ace76e5` 的充分反证。

## 8. 排除项

### 8.1 不是再次发生 64 KiB 单栈溢出

单栈已经在 `ebd27f76` 扩到 128 KiB，`clone09` 随后通过。本案 panic 位于新 slot
地址计算的编号检查，不是 guard-page fault。

### 8.2 不是 heap 或物理内存 OOM

保留日志在 panic 时仍报告约 141 MiB heap free、49,122 physical frames free。固定 VA
slot 用完与物理内存用完是两种资源。

### 8.3 不是 `futex_cmp_requeue()` 返回值错误

panic 前的 requeue 子项报告 TPASS。futex 是高强度生命周期放大器，不等于 futex
语义本身是这个资源泄漏的根因。

### 8.4 不能只把上限改成 2048

扩大上限会推迟 panic，却不修复“quota 已释放、TCB 仍被强引用”的不变量破坏；还会
扩大保留虚拟地址窗口，并让同类泄漏更晚、更难复现。

## 9. 后续应补的硬化

1. 将 `kstack_alloc()` 改为 `Result<KernelStack, ...>`，在 clone 路径返回 Linux 语义的
   `EAGAIN` 或 `ENOMEM`，不让用户压力 panic 内核。
2. 维护 `live_kstack_slots` 和 high-water，和 quota、ready、interruptible、zombie
   计数一起输出。
3. 增加“反复 1000 waiter、wait 完后再创建”的多轮压力，检查 slot id 是否回落/复用。
4. 为 boot/init/内核后台任务预留硬 headroom，不让用户 quota 正好等于全部 1024 slot。
5. 审计所有 TCB 强引用容器；修复 zombie 队列不代表 timer、wait queue、registry 中
   不可能存在另一条生命周期滞留路径。

## 10. 闭合证据链

```text
长序列 futex waiter 压力
  -> clone quota 报 921/1024
  -> 新栈分配命中 slot id 1024 并 panic
  -> panic 类型排除单栈溢出和普通 OOM
  -> wait_child 已释放 PID/quota，并调用指定 PID 清理
  -> 修复前清理只覆盖 ready + interruptible
  -> 专用 zombie_queue 仍保存强 Arc<TCB>
  -> TCB 持有 KernelStack，最后一个 Arc 不消失就不会 Drop
  -> quota 可下降而 stack slot 不归还
  -> 1ace76e5 将 zombie_queue 纳入同一锁内摘除、锁外析构协议
  -> 实板双 libc 长序列与两轮 1000 waiter 不再触发该泄漏路径
  -> 但 1024 硬上限与 infallible allocation 仍保留为明确边界
```

组会汇报时应把结论说成：**修复的是 reap 与 zombie 队列不对称导致的 stack slot
滞留；不是靠增大栈，也不是已经取消 1024 容量上限。**
