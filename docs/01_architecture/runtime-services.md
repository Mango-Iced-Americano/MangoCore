---
title: "运行期服务 (Runtime Services)"
category: architecture
status: stable
author: MangoCore Team
last_update: 2026-08-01
tags: [architecture, runtime, trace, timer]
---

# 运行期服务

## 1. 概述

运行期服务覆盖不属于单一业务子系统、但在内核执行期间持续参与控制流的基础能力：

| 服务 | 主要文件 | 作用 |
|------|----------|------|
| console/log | `os/src/console.rs` | 内核输出、日志等级、带任务信息的日志前缀 |
| trace | `os/src/trace.rs` | trace ring buffer、Ctrl+T dump、TTY 字符暂存 |
| perf/stat | `os/src/task/perf.rs`、`task/processor.rs` | syscall、调度、timer、TLB、PageCache 等计数 |
| timer | `os/src/timer.rs`、`hal/arch/*/time.rs`、`task/manager.rs` | 时间结构、硬件 timer、等待超时 |
| scheduler maintenance | `os/src/task/processor.rs` | 网络 poll、FS reclaim、zombie drain、futex compact |
| panic/shutdown | `os/src/panic_diag.rs`、HAL shutdown | panic 诊断和 QEMU/平台退出 |

这些服务不是启动时一次性完成。`task::run_tasks()` 每轮循环都会驱动其中一部分维护动作。

## 2. Console 与日志

### 2.1 输出路径

`console.rs` 的 `KernelOutput` 实现 `core::fmt::Write`。正常输出顺序固定为：

```text
local_irq_save
  -> OUTPUT_LOCK
     -> HAL console_write_bytes
        -> LA64 UART Mutex（RV64 无第二层 Rust 锁）
     <-
  <- OUTPUT_LOCK
local_irq_restore
```

关本地中断只防止同 CPU 重入，`OUTPUT_LOCK` 才负责跨 CPU 串行化一次完整的
`print!` 或 TTY write。logger 把颜色前缀、正文和 reset 合并成一次 `println!`，避免三个
独立临界区之间被其它 CPU 插入。LA64 对整个字节切片只取得一次 UART 锁，并在每个字节前
等待 NS16550 THR ready；RV64 QEMU 继续使用直接 MMIO 批量路径。

panic handler 在第一次输出前执行 `console::enter_panic()`。该单向原子状态使等待
`OUTPUT_LOCK` 的 CPU 放弃可能永不释放的 owner；panic 输出直接调用 HAL raw writer，
不取得 `OUTPUT_LOCK` 或 LA64 UART Mutex。raw writer 仍可能等待真实 UART/SBI 发送就绪，
“无锁”不等于“硬件非阻塞”。

### 2.2 日志初始化

`console::log_init()` 注册 `Logger` 并按编译环境变量 `LOG` 设置等级：

| `LOG` | 等级 |
|-------|------|
| `error` | `LevelFilter::Error` |
| `warn` | `LevelFilter::Warn` |
| `info` | `LevelFilter::Info` |
| `debug` | `LevelFilter::Debug` |
| `trace` | `LevelFilter::Trace` |
| 其他/未设置 | `LevelFilter::Off` |

日志输出前缀来自当前任务：

| 当前任务 | 格式 |
|----------|------|
| 有任务 | `[sec.msec] tid T pid P: message` |
| 无任务 | `[sec.msec] kernel: message` |

颜色由日志等级决定：error 红色，warn 亮黄，info 蓝色，debug 绿色，trace 亮黑。

## 3. Trace ring buffer

### 3.1 数据结构

`trace.rs` 定义：

```rust
pub(crate) const TRACE_SIZE: usize = 2048;
pub const MAGIC_KEY: u8 = 0x14;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceEntry {
    pub timestamp: u64,
    pub tag: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
    pub arg6: u64,
}
```

trace buffer 是 2048 项环形数组，由 `Mutex<RingInner>` 保护。每项包含微秒时间戳、tag 和六个 payload 字段。

### 3.2 开关与丢弃计数

| 全局项 | 作用 |
|--------|------|
| `TRACING_ON: AtomicBool` | 运行期开关，可由 sysfs tracing 文件控制 |
| `TRACE_DROPPED: AtomicUsize` | tracing 关闭或 ring 满时的丢弃计数 |
| `DUMP_LOCK: AtomicBool` | 防止 trace dump 重入 |

### 3.3 syscall return tag

`TRACE_RET_MASK = 0x8000_0000_0000_0000`。当 `entry.tag & TRACE_RET_MASK != 0` 时，trace 输出把它解释为 syscall return 事件，syscall id 是 `tag & !TRACE_RET_MASK`。

普通 tag 会先尝试 `syscall::syscall_name(tag as usize)`，若不是 syscall 再进入自定义 tag 映射。例如网络/连接相关 tag 包括 `0xB031`、`0xB032`、`0xB036` 等。

### 3.4 调度循环中的 Ctrl+T

`task::run_tasks()` 会轮询 console 字符。rv64 路径每 64 个 schedule tick 轮询一次，其他架构每轮轮询：

```rust
#[cfg(target_arch = "riscv64")]
let should_poll_console = schedule_tick % RV64_CONSOLE_POLL_INTERVAL == 0;
#[cfg(not(target_arch = "riscv64"))]
let should_poll_console = true;
```

字符处理顺序：

| 字符类型 | 行为 |
|----------|------|
| Ctrl+T (`0x14`) | `trace::check_magic_key(ch, "schedule")` 触发 trace dump 和 shutdown |
| VINTR | `Teletype::handle_vintr(ch)` 向前台/阻塞任务发送 SIGINT |
| 普通字符 | `trace::stash_char(ch)` 保存给 TTY，随后唤醒 TTY reader |

`CharStash` 是 128 字节环形缓冲区，调度循环读到的非 magic 字符会暂存，TTY read 路径先从 stash 消费。

## 4. 调度循环维护动作

`task/processor.rs::run_tasks()` 每轮执行的维护顺序：

```
schedule_tick += 1
console poll                       [rv64 每 64 tick，其他架构每 tick]
do_wake_expired()
NET_INTERFACE.try_poll()           [每 64 tick]
fs::reclaim::maybe_reclaim_fs_caches()
zombie queue drain
stale zombie cleanup + queue stats [每 64 tick]
threads::compact_shared_futex()
fetch_task()
switch or idle
```

### 4.1 网络 poll

| 场景 | 行为 |
|------|------|
| 有调度循环进展 | 每 `BACKGROUND_NET_POLL_INTERVAL = 64` tick 调用 `NET_INTERFACE.try_poll()` |
| 没有 ready task | 每 `IDLE_NET_POLL_INTERVAL = 64` tick 调用 `NET_INTERFACE.poll()`，否则 `spin_loop()` |

后台 poll 使用 `try_poll()` 避免阻塞在网络接口锁上；idle 路径允许调用 `poll()` 驱动网络进展。

### 4.2 timeout 唤醒

`do_wake_expired()` 在每轮循环执行。源码注释说明这是 legacy timeout sweep，保留到所有等待路径都证明完全由 timer interrupt 驱动为止；移除它可能让早期启动网络等待滞留。

### 4.3 FS reclaim

每轮调用 `fs::reclaim::maybe_reclaim_fs_caches()`。该入口负责按文件系统缓存策略尝试回收 page cache 等资源。

### 4.4 zombie drain

退出任务先进入专用 zombie 队列。调度循环发现 `has_zombie_queue_tasks_fast()` 后，批量取出最多 64 个 zombie 并 drop，避免不可运行的 TCB 留在 ready queue。

每 64 tick 还会执行兜底清理：从 ready 和 interruptible 队列各尝试取出 zombie，并记录 ready/interruptible/zombie/nonzero nice 统计。

### 4.5 shared futex compact

每轮调用 `threads::compact_shared_futex()`，降低 `PROCESS_SHARED_FUTEX` 中空 WaitQueue key 长期残留的概率。

## 5. Per-CPU 当前任务状态

每个 `PerCpu` 内嵌一个 `CpuTaskState`，其中本地 `Processor` 保存 current `Arc`
和 idle context。`current_task()` 根据 CPU-local 寄存器选择本核槽位，在锁内克隆
`Arc` 后立即释放锁，不再通过全局裸指针伪造任务生命周期。

只有 PID、TID 和诊断用 syscall ID 保留为 Per-CPU 原子快照。父 PID、身份、
进程组、会话和用户页表 token 可能在任务运行期间改变，因此直接从 TCB/PCB 的
权威原子 hint 读取。任务真实切回 idle 栈后，`finish_current_switch_out()` 才清空
本核 current 槽和快照。

panic 路径使用不阻塞的 `try_current_task()`；CPU-local 尚未安装或 processor 锁
正被占用时只报告不可用。任何本地 current `Arc` 都必须在 `schedule()` 或架构
`noreturn` 返回路径前显式释放，因为 context switch 不会展开旧 Rust 栈帧。

## 6. 调度 profiling

`processor.rs` 内部有调度 debug profile 计数：

| 计数 | 含义 |
|------|------|
| `SCHED_LOOPS` | 调度循环次数 |
| `SCHED_FETCH` / `SCHED_IDLE` | 有任务/无任务分支计数 |
| `SCHED_SWITCHES` | 上下文切换次数 |
| `SCHED_STAGE_*_CALLS` | console、wake、net poll、reclaim、zombie、futex、fetch、idle 等阶段调用次数 |
| `SCHED_STAGE_*_CYCLES_TOTAL/MAX` | 对应阶段耗时统计 |
| `SCHED_TIMER_*` | timer trap、handler、program timer、SBI set timer 相关耗时 |

`sched_rdcycle()` 在 rv64 使用 `rdcycle`，在 la64 使用 `rdtime.d`。

## 7. task/perf 统计

`task/perf.rs` 定义 `STATS_ON` 作为运行期开关。未启用 `perf_stats` feature 时，`stats_enabled()` 恒为 false；启用 feature 后，sysfs 可控制计数是否生效。

统计类别包括：

| 类别 | 代表计数 |
|------|----------|
| clone/exit | `CLONE_TOTAL`、`EXIT_THREAD`、`EXIT_CLEAR_CHILD_TID` |
| 调度队列 | `ADD_READY_TOTAL`、`READY_LEN_MAX`、`FAIR_PICK_CALLS` |
| timer | `KTIMER_ADD_TOTAL`、`TIMER_IRQ_TICKS_TOTAL`、`WAIT_WITH_TIMEOUT_TOTAL` |
| seccomp/syscall/trap | `SECCOMP_CHECK_CALLS`、`SYSCALL_TOTAL`、`ECALL_TRAP_COST_TICKS_TOTAL` |
| TLB | `TLB_FLUSHES`、`TLB_FULL`、`TLB_PAGE`、`TLB_ACTIVATE`、`TLB_GLOBAL` |
| MM | `FRAME_ALLOC_HITS`、`PAGE_FAULTS`、`PAGEFAULT_TIME_TICKS` |
| FS/PageCache | `PC_READ_CALLS`、`PC_WRITE_CALLS`、`PC_WRITEBACK_CALLS` |
| block I/O | `BLK_VREAD_REQS`、`BLK_VWRITE_REQS` |
| heap | `HEAP_ALLOC_CALLS`、`HEAP_DEALLOC_CALLS` |

这些计数用于性能诊断，不改变业务语义。

## 8. Timer 服务

timer 能力分三层：

| 层 | 文件 | 作用 |
|----|------|------|
| HAL time | `hal/arch/*/time.rs` | 读取硬件时间、获取频率、设置 timer delta |
| 通用时间结构 | `os/src/timer.rs` | `TimeSpec`、`TimeVal`、时间单位转换 |
| task timer | `os/src/task/manager.rs` | sleep、timeout、futex/wait queue timer、POSIX timer 基础 |

trap 后端收到 timer interrupt 后进入 `task::timer_interrupt_handler()`。调度循环中的 `do_wake_expired()` 负责处理到期等待者，形成 timer interrupt 与调度循环的双入口保障。

## 9. shutdown 与 panic

| 入口 | 行为 |
|------|------|
| `hal::shutdown()` | 由架构后端实现平台退出 |
| `trace::check_magic_key()` | Ctrl+T 触发 trace dump 后 shutdown |
| `panic_diag` | panic 诊断输出 |
| `SYSCALL_SHUTDOWN` | MangoCore 非标准系统调用，进入系统关机路径 |

普通日志通过 irq-save 全局锁输出；panic 先关闭本地中断，再永久切换到不等待内核
console/UART 锁的 raw 路径。panic 诊断因此可以在普通输出临界区自身发生崩溃时继续打印。

## 10. 调试入口

| 症状 | 首选检查点 |
|------|------------|
| 日志没有输出 | `console::log_init()`、`LOG` 环境变量、HAL console |
| Ctrl+T 无效 | `run_tasks()` console poll、`trace::check_magic_key()` |
| TTY 丢字符 | `trace::stash_char()`、`CharStash` 容量和 TTY reader |
| 网络不进展 | `BACKGROUND_NET_POLL_INTERVAL`、idle `NET_INTERFACE.poll()` |
| timeout 不醒 | `do_wake_expired()`、timer interrupt handler |
| zombie 堆积 | zombie queue drain、stale zombie cleanup |
| perf 无计数 | `perf_stats` feature 和 `STATS_ON` |

运行期服务大多没有独立内核线程，而是挂在调度循环、trap 返回或 syscall 公共路径上。trace 事件在 syscall 入口记录，timer 到期在调度循环和中断路径推进，网络 poll 在调度循环降频执行，PageCache reclaim 也通过调度循环合作式调用。调试运行期服务时要先确认触发点是否被执行，再看服务内部状态。

例如 timeout 不醒不能只看 `KernelTimerQueue` 是否插入了 timer，还要看 timer interrupt 是否推进时间、`do_wake_expired()` 是否在调度循环执行、等待任务是否仍在 interruptible queue。网络不进展也不能只看 socket 对象，还要看网卡是否初始化、`NET_INTERFACE.try_poll()` 是否周期执行、idle 分支是否调用 `NET_INTERFACE.poll()`。

## 11. 测试映射

| 测试目标 | 覆盖服务 |
|----------|----------|
| `LOG=info/trace` 启动 | console/log |
| Ctrl+T trace dump | trace ring buffer、scheduler console poll |
| busybox shell 输入 | CharStash、TTY wake reader |
| nanosleep/futex timeout | HAL timer、task timer、do_wake_expired |
| 网络 busybox/iperf | scheduler background net poll |
| fork/exit 压力 | zombie queue drain、task perf |
| PageCache 压力 | FS reclaim、PageCache perf |

## 12. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/console.rs` | print/log 实现 |
| `os/src/trace.rs` | trace ring buffer、magic key、char stash |
| `os/src/task/processor.rs` | 调度循环、当前任务缓存、scheduler profiling |
| `os/src/task/perf.rs` | perf 统计开关和计数 |
| `os/src/task/manager.rs` | task timer 和 timeout 队列 |
| `os/src/timer.rs` | 通用时间结构 |
| `os/src/hal/arch/riscv/time.rs` | rv64 timer |
| `os/src/hal/arch/loongarch64/time.rs` | la64 timer |
| `os/src/panic_diag.rs` | panic 诊断 |
