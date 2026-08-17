---
title: "L3 — 内核态 Self-Test"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, l3, ktest, kernel-test, tap, qemu]
---

# L3 — 内核态 Self-Test

L3 是内核态自检：测试代码**编译进内核**，在 QEMU 内以 `mango.mode=ktest` 运行，通过 TAP 输出结果。它是测试体系的核心创新。

## 设计

L3 测的是**真实内核机制**——WaitQueue、timer、scheduler、页分配器、SMP 调度等。测试代码编译进内核，但只在 `mango.mode=ktest` 时运行：内核完成全部子系统初始化后，不启动用户态 init，直接进入测试运行器，执行完毕后通过 HAL `shutdown()` 退出。

在 L0-L5 体系中，L3 承担**内核机制正确性**的验证：能在 L3 解决的不拖到 LTP。它比 L1 更接近真实运行环境（真实调度、真实内存、真实 SMP），又比 L4/L5 更聚焦（不经过用户态 ABI 和完整 rootfs）。

### 启动流程

```
rust_main()
  → bootstrap_init() → mem_clear() → console::log_init()
  → trace::init() → mm::init()
  → machine_init() → timer_cpu_init(CPU0) → bring_up_secondary_cpus()
  → [fs init, net init, block probe, preload payloads]
  → posix_lock::init()
  → smp::release_secondary_schedulers()
  → AP: activate kernel page table → timer_cpu_init(AP) → run_tasks()
  → if mode == Ktest: spawn kernel test runner → per-CPU run_tasks() → shutdown()
  → normal: add_initproc() → per-CPU run_tasks()
```

ktest 分支位于 `add_initproc()` 之前，因此不会创建 PID1；进入该分支前文件系统、网络、块设备和任务 registry 已初始化，scheduler-ready 已发布。runner 固定 CPU0，SMP focused 测试可创建受控的 AP kernel-only 任务。

## 原理

L3 依赖 `mango.mode=ktest` bootargs + `kernel_tests` runner + TAP 输出：

- **bootargs 注入**：Makefile 在编译时通过 `MANGO_CMDLINE` 环境变量注入 bootargs，内核通过 `option_env!("MANGO_CMDLINE")` 读取。Ktest 模式的 QEMU 启动不挂载磁盘镜像，仅需内核二进制。
- **runner**：`os/src/kernel_tests/runner.rs` 负责测试选择、repeat、timeout、failfast 和 TAP 输出。
- **TAP 输出**：兼容标准测试消费者，失败时 YAML block 包含 `reason` 和 `elapsed_ms`。

### 目录结构

```
os/src/kernel_tests/
├── mod.rs            # 注册所有测试组，run_from_bootargs() 入口
├── runner.rs         # TAP 输出、timeout/repeat/failfast
├── waitqueue.rs      # WaitQueue 测试注册与基础队列用例
├── waitqueue_blocking.rs  # 阻塞、deadline、多队列、陈旧 waiter
├── waitqueue_wake.rs      # FIFO、wake_all、1000-cycle 压力
├── waitqueue_interrupt.rs # 信号中断与 signal/wake race
├── timer.rs          # tick_advances, time_spec_ops
├── sched.rs          # current_task_exists, ready_queue_has_init
├── smp.rs            # online/IPI/AP 调度、受控用户 trap/exit、TLB/ASID、STOP
├── mm.rs             # alloc_free_one_page, alloc_contiguous_pages
└── ext4.rs           # TestMemBlock + ext4 多实例挂载隔离
```

### 测试项结构

```rust
pub struct KernelTest {
    pub name: &'static str,       // "waitqueue::wake_once"
    pub func: fn() -> Result<(), &'static str>,
    pub timeout_ms: usize,        // 0 = use global default
    pub terminal: bool,           // true = 整个测试计划末尾只执行一次
}
```

### Runner 行为

| 特性 | 说明 |
|------|------|
| 测试选择 | 根据 `mango.test=waitqueue,sched` 过滤测试组；`all` 跑全部 |
| repeat | `mango.test.repeat=N`，每个测试重复 N 次（抓偶发 bug） |
| terminal | 普通测试全部 repeat 完成后执行一次；用于 STOP 等不可恢复测试 |
| timeout | `mango.test.timeout_ms=N`，全局超时；测试可覆盖 |
| failfast | `mango.test.failfast=1`，遇第一个失败即停 |
| arch 诊断 | 输出 `# arch: riscv64` / `loongarch64` 用于 CI 区分 |

**限制**：当前 timeout 是 advisory-only — 在测试函数返回后检查耗时，无法中断挂死测试。需要后续添加 watchdog timer 才可实现抢占式超时。

永久停止 AP、关机或不可逆破坏全局状态的用例必须用 `KernelTest::terminal(name, func)` 注册。runner 会先执行所有选中组的普通测试及其 repeat，最后才执行 terminal 集合；terminal 不参与 repeat。因此 `KTEST=all` 不会因 SMP STOP 提前破坏后续 MM/FS 测试，`KREPEAT>1` 也不会尝试再次唤醒已经停止的 AP。

### TAP 输出格式

```
TAP version 13
# arch: riscv64
# mode: ktest
# repeat: 1
# timeout_ms: 5000
# failfast: false
1..5
ok 1 waitqueue::wake_before_wait_should_not_sleep
ok 2 waitqueue::basic_queue_ops
ok 3 timer::tick_advances
ok 4 timer::time_spec_ops
not ok 5 sched::ready_queue_has_init
  ---
  reason: no ready tasks after add_initproc()
  elapsed_ms: 0
  ...
# results: 4 passed, 1 failed, 5 total
# ktest: tests FAILED. shutting down.
```

### 当前测试清单（19 个）

| 测试 | 文件 | 说明 |
|------|------|------|
| `mm::alloc_free_one_page` | `mm.rs` | 分配单页 → 释放 → 验证 PPN 有效 |
| `mm::alloc_contiguous_pages` | `mm.rs` | 分配 4 连续页 → 计数与连续性校验 → 释放 |
| `mm::alloc_then_free_then_alloc` | `mm.rs` | 分配 8 页 → 释放 → 再分配 8 页（复用验证） |
| `sched::current_task_exists` | `sched.rs` | 验证 `add_initproc()` 后 `task_manager_counts()` 返回 ready>0 |
| `sched::ready_queue_has_init` | `sched.rs` | 验证 `add_initproc()` 后 `has_ready_task()` |
| `sched::task_manager_counts` | `sched.rs` | 验证 ready/interruptible 计数在合理范围 |
| `timer::tick_advances` | `timer.rs` | busy-wait 后时间严格递增 (`t1 > t0`) |
| `timer::time_spec_ops` | `timer.rs` | TimeSpec 构造精度、进位加法、减法钳位、跨单位等价、偏序、is_zero |
| `timer::now_monotonic` | `timer.rs` | 两次 `now()` 验证单调不倒退 |
| `waitqueue::wake_before_wait_should_not_sleep` | `waitqueue.rs` | 条件已满足时 `wait_until` 立即返回正确值 |
| `waitqueue::early_wake_cancels_block` | `waitqueue.rs` | waiter 已登记但尚未 Blocking 时，通知 token 可撤销阻塞 |
| `waitqueue::condition_can_notify_same_queue` | `waitqueue.rs` | 登记后条件检查可可靠通知同一队列，无自锁或丢 wake |
| `waitqueue::basic_queue_ops` | `waitqueue.rs` | 新建队列 → is_empty → compact_stale → is_empty |
| `waitqueue::wake_all_on_empty` | `waitqueue.rs` | 空队列 `wake_all()` 返回 0 |
| `waitqueue::wake_one` | `waitqueue.rs` | 真实调度下阻塞 waiter 被另一个内核任务唤醒 |
| `waitqueue::basic_block_wake` / `no_spurious_wake_without_fallback` | `waitqueue_blocking.rs` | 条件驱动的阻塞/唤醒，以及无显式唤醒、信号或 deadline 时 200ms 内持续阻塞 |
| `waitqueue::multi_queue_cleanup` / `deadline_timeout` / `stale_waiter_cleanup` | `waitqueue_blocking.rs` | 双队列清理、deadline 及失效 weak waiter |
| `waitqueue::wake_one_fifo` / `wake_all_wakes_all` / `thousand_cycle_stress` | `waitqueue_wake.rs` | FIFO 单唤醒、广播和 1000 次无丢失/重复入队压力 |
| `waitqueue::signal_interrupt` / `signal_wake_race` | `waitqueue_interrupt.rs` | 信号中断与 Ready 优先于同时到达信号的 race 语义 |
| `ext4::memblk_read_write` | `ext4.rs` | `TestMemBlock` BlockDevice 读写正确性 |
| `ext4::memblk_isolation` | `ext4.rs` | 两个独立 `TestMemBlock` 实例的数据不互泄露 |
| `ext4::open_unformatted_returns_err` | `ext4.rs` | 未格式化设备上 `open_ext4rs` 返回错误（不 panic） |
| `ext4::lw_path_isolation` | `ext4.rs` | lwext4 `lw_path()` 路径翻译的实例隔离语义 |

**规划中**（需要内核线程 spawn API、更丰富的 ktest task 参数传递或格式化块设备）：
- `sched::spawn_and_yield` — 创建线程 → yield → 验证运行
- 按任务定向注入信号（当前信号测试使用唯一 interruptible ktest worker）
- `timer::sleep_returns` — 真正阻塞等待 deadline
- `fs::tmpfs_create_write_read_unlink` — VFS 基础路径
- `pagecache::basic_insert_lookup_evict` — 页缓存操作

## 如何启动运行

所有命令在 **Docker 容器内**执行（`make docker` 进入）：

```bash
# 跑全部 L3 测试（rv64）
make rv64-ktest

# 指定测试组
make rv64-ktest KTEST=waitqueue

# 压力测试（重复 1000 次抓偶发 bug）
make rv64-ktest KTEST=waitqueue KREPEAT=1000

# 打开 trace
make rv64-ktest KTEST=sched KTRACE=waitqueue,sched

# 跨架构对照（la64）
make la64-ktest KTEST=all
```

等价 Makefile 入口（在 `os/` 目录）：

```bash
make -C os rv64-ktest                # rv64 全部 L3
make -C os rv64-ktest KTEST=waitqueue KREPEAT=100
```

### 添加新的 L3 测试

1. 在 `os/src/kernel_tests/` 下创建 `my_subsystem.rs`
2. 实现 `pub fn tests() -> Vec<KernelTest>`
3. 在 `mod.rs` 中注册：`#[path = "my_subsystem.rs"] mod kt_my;` 并在 `all_tests()` 中添加条目
4. 确保测试函数 compute-bounded（不无限阻塞），失败路径返回 `Err("reason")`
