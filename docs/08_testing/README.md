---
title: "测试体系 (Testing Framework)"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-07-30
tags: [testing, ktest, cargo-test, LTP, regression, tap]
---

# 测试体系

## CI 评分门禁

`develop` 和 `main` 共用 [统一 CI 与 L5 评分](ci-scoring.md)：Docker Compose 中串行执行 RV64/LA64 QEMU，归档原始日志，并按 11 个 musl/glibc 组输出结构化评分 JSON。

## 概述

MangoCore 采用五层自底向上的测试体系，从纯逻辑单元测试到内核自检、再到用户态回归测试和官方集成测试，建立完整的 bug 扫描工具链。目标是把问题定位逐步下沉——能在 `cargo test` 解决的不拖到 QEMU，能在 L3 解决的不拖到 LTP。

## 快速开始

所有测试命令在 **Docker 容器内**执行（`make docker` 进入）：

```bash
# 根目录评测入口会按需 provision；全新容器首次运行可能使用网络
cd /app
make all

# 直接运行 OS、用户态或架构目标前，先只读检查
make toolchain-preflight

# 手动/direct workflow 仍可显式准备工具链
make toolchain-setup

# ── L1: 纯逻辑单元测试（秒级，host 上跑）──
cd /app
cargo test -p mango-kernel-core   # 148 个测试

# ── L3: 内核自检（分钟级，QEMU 内跑）──
cd /app/os
make rv64-ktest                    # 全部 L3 测试
make rv64-ktest KTEST=waitqueue    # 指定模块
make rv64-ktest KTEST=all KREPEAT=100  # 压力测试

# ── L4: 用户态回归（分钟级，QEMU 内跑）──
cd /app
make regression

# ── 一键全扫 ──
make bugscan                       # L1 + L3
```

> ⚠️ **不要**在 `os/` 或 `user/` 目录下直接跑 `cargo test`——它们是 `#![no_std]` 裸机 crate，host 上无法编译测试。L1 的 `cargo test` 只能在项目根目录通过 `-p mango-kernel-core` 指定纯逻辑库 crate。

```
cargo test (L1/L2)       →  判断纯逻辑模块是否正确
ktest / L3               →  判断真实内核机制是否正确
user regression / L4     →  判断用户态可见行为是否正确
LTP/lmbench/official /L5 →  判断系统兼容性、性能和比赛表现
```

## 依据范围

| 主题 | 主要源码 |
|------|----------|
| L1 纯逻辑库 crate | `libs/mango-kernel-core/src/` |
| L1 os 侧 wrapper | `os/src/bootargs.rs` |
| L3 测试框架入口 | `os/src/kernel_tests/mod.rs` |
| L3 测试运行器 | `os/src/kernel_tests/runner.rs` |
| L3 waitqueue 测试 | `os/src/kernel_tests/waitqueue.rs` |
| L3 timer 测试 | `os/src/kernel_tests/timer.rs` |
| L3 scheduler 测试 | `os/src/kernel_tests/sched.rs` |
| L3 页分配器测试 | `os/src/kernel_tests/mm.rs` |
| ktest 启动分支 | `os/src/main.rs` (`add_initproc()` 之后) |
| ktest Makefile 目标 | `os/Makefile`, `os/make/rv64.mk`, `os/make/la64.mk` |
| 工具链固定与检查 | `rust-toolchain.toml`, `scripts/rustup-{setup,preflight}.sh` |
| L5 测试配置与注入 | `os_test.conf`, `os/Makefile` (`conf-inject`) |
| L5 测试脚本 | `scripts/run_full_test.py` |

## 分层总览

```
L0: 编译与静态检查
    cargo check  |  cargo fmt --check  |  cargo clippy
    → 秒级反馈，CI 第一道关卡

L1: 纯逻辑单元测试
    cargo test -p mango-kernel-core
    → 无内核依赖，host 上运行。当前覆盖：7 个模块，147 个用例

L2: 属性测试 / 模型测试 (规划中)
    proptest 页缓存状态机  |  loom 并发 waitqueue
    → 同 L1 机制，未来引入

L3: 内核态 self-test
    mango.mode=ktest  |  QEMU 内运行  |  TAP 输出
    → 不启动用户态 init。当前覆盖：waitqueue / timer / sched / mm / ext4 (16 个用例)

L4: 用户态 regression test
    user/src/bin/regression_*.rs  |  make regression
    → 每个 bug 沉淀一个最小复现程序。initproc 新增 RunMode::Regression，配置文件注入 `mode=regression`，initproc fork+exec `/regression` → 打印 `[L4 REGRESSION PASSED/FAILED]` → shutdown

L5: 官方集成测试
    LTP / lmbench / iperf / libc-test / 比赛测例
    → 最终验收和性能趋势观察，通过 os_test.conf mask 控制范围
```

---

## L0 — 编译与静态检查

### 入口

```bash
make check-fast
```

### 覆盖

| 检查 | 命令 | 耗时 |
|------|------|------|
| 类型检查 | `cargo check` | ~15s |
| 格式检查 | `cargo fmt --check` | ~2s |
| Lint | `cargo clippy` | ~30s |

编译器由根 `rust-toolchain.toml` 固定。根目录 `make all` 会派生 HOME 对应的 `RUSTUP_HOME` 和 `CARGO_HOME`，并在需要时执行 setup 和 preflight。直接 OS、用户态或架构目标只做 preflight，不自动安装 Rustup 工具链。

---

## L1 — 纯逻辑单元测试 (`cargo test`)

### 设计原则

L1 和 L2 在逻辑上分层，但都走 `cargo test`。L1 测确定性逻辑（解析、算术、状态转换），L2 测随机性质（proptest）或并发模型（loom）。

纯逻辑模块被提取到独立库 crate，在 host 上编译和测试。内核通过 path dependency 引用同一份源码，不维护两份副本。

### 库 crate

```
libs/mango-kernel-core/
├── Cargo.toml          # #![no_std] lib, host-testable
└── src/
    ├── lib.rs           # extern crate alloc; pub mod bootargs; ...
    ├── bootargs.rs      # Cmdline, BootConfig, BootMode + #[cfg(test)]  (28 tests)
    ├── time.rs          # TimeSpec, TimeVal, ItimerVal + #[cfg(test)]   (50 tests)
    ├── page_cache.rs    # PageState, RAMask, ReadAhead + #[cfg(test)]   (25 tests)
    ├── ring_buffer.rs   # Bounded VecDeque-backed ring buffer           (11 tests)
    ├── path.rs          # Path normalization with '.'/'..' resolution   (12 tests)
    ├── wait_result.rs   # WaitQueue result enum + errno encoding         (7 tests)
    └── recycle_alloc.rs # Recyclable ID allocator (PID/TID)             (14 tests)
```

`lib.rs` 是标准 `#![no_std]` 库入口。测试时 Cargo 自动注入 `std` 和 test harness，源码中的 `extern crate alloc` 在 host 测试下正常工作。

### 执行

```bash
cargo test -p mango-kernel-core
```

### 当前覆盖 (147 个用例)

| 模块 | 文件 | 用例数 | 说明 |
|------|------|--------|------|
| bootargs | `bootargs.rs` | 28 | Cmdline 解析、BootMode、BootConfig、参数验证 |
| time | `time.rs` | 50 | TimeSpec/TimeVal 算术、构造、比较、钳位 |
| page_cache | `page_cache.rs` | 25 | PageState、RAState、segments/mask 操作 |
| ring_buffer | `ring_buffer.rs` | 11 | 有界队列 push/pop/slice/shutdown 语义 |
| **path** | `path.rs` | **12** | 路径分词、`.` `..` 标准化、连续斜线归一化 |
| **wait_result** | `wait_result.rs` | **7** | Ready/Interrupted/TimedOut 与 errno 编码 |
| **recycle_alloc** | `recycle_alloc.rs` | **14** | ID 分配/回收、fresh vs 回收优先、水位线行为 |

### 添加新的 L1 测试

1. 将纯逻辑模块移动到 `libs/mango-kernel-core/src/`
2. 在 `lib.rs` 中 `pub mod my_module;`
3. 在模块底部加 `#[cfg(test)] mod tests { ... }`
4. 如模块被内核引用，在 `os/src/` 中创建 wrapper re-export

判断标准：模块**零 arch 依赖**、**零全局状态**、**零 I/O** — 纯 `String → Struct` 转换、算法、状态机均可。

---

## L2 — 属性测试 / 模型测试

### 规划

| 目标 | 工具 | 场景 |
|------|------|------|
| PageCache 状态机 | `proptest` | 随机操作序列验证 dirty/clean/evict 状态一致性 |
| Dentry tree | `proptest` | 随机 lookup/create/unlink 验证树结构不变式 |
| WaitQueue 并发 | `loom` | 多线程 wake/wait 交错验证无丢唤醒 |
| Pipe buffer | `loom` | reader/writer 并发验证数据完整性和阻塞语义 |

### 机制

与 L1 相同，所有依赖加入 `libs/mango-kernel-core` 的 `[dev-dependencies]`，不影响内核编译。当前阶段接口已就绪，具体测试用例待后续迭代。

---

## L3 — 内核态 Self-Test

### 设计

L3 是测试体系的核心创新。测试代码**编译进内核**，但只在 `mango.mode=ktest` 时运行——内核完成全部子系统初始化后，不启动用户态 init，直接进入测试运行器，执行完毕后通过 HAL `shutdown()` 退出。

### 启动流程

```
rust_main()
  → bootstrap_init() → mem_clear() → console::log_init()
  → trace::init() → mm::init()
  → machine_init() → timer_subsystem_init()
  → [fs init, net init, block probe, preload payloads]
  → posix_lock::init()
  → smp::release_secondary_schedulers()
  → if mode == Ktest: spawn kernel test runner → per-CPU run_tasks() → shutdown()
  → normal: add_initproc() → per-CPU run_tasks()
```

ktest 分支位于 `add_initproc()` 之前，因此不会创建 PID1；进入该分支前文件系统、网络、
块设备和任务 registry 已初始化，scheduler-ready 已发布。runner 固定 CPU0，SMP focused
测试可创建受控的 AP kernel-only 任务。B28 另有一个 hermetic 用户探针：CPU0 构造并
发布到 CPU1，依次触发 getpid、yield 和非返回 exit，再由 CPU0 wait/reap。B29 将该用例
升级为先发布 CPU0、在真实 yield 后迁移到 CPU1，并覆盖两个 CPU 的 MM shootdown；它不进入
FS/net/driver，也不表示普通用户任务已开放多核调度。B30 又在同一探针内调用真实 getcpu：
yield 前必须写出逻辑 CPU0，yield 返回后必须写出逻辑 CPU1。任一 syscall 错误、固定返回 0、
未迁移或错误起跑都会转换为 exit(1)，因此不能只依赖 runner 观察的 `last_cpu` 间接判定。
B31 不增加新的 TAP 名称，而是让现有三个生产路径用例同时验证
`cpus_allowed`：`remote_kernel_tasks_run_on_target_cpus` 覆盖定向首次发布，
`blocked_kernel_tasks_wake_on_last_cpu` 覆盖唤醒重新入队，
`user_task_migrates_on_yield` 覆盖 CPU0/CPU1 mask 下的 owner 交接。
B32 继续复用第 20 项，但 user probe 现在还在迁移前用正 ID、迁移后用 `pid=0`
调用 raw `sched_getaffinity`；两次都必须返回 8 并写出 `0b11`，否则进程 exit(1)。
probe 是单线程 leader，正 ID 同时等于 PID/TID，所以非 leader TID 的严格查找需要结合
`ProcessManager::find_task(tid)` 源码审计，不能只靠 TAP 总数声称覆盖。
B33 将该项改名为 `smp::user_task_reschedules_from_ipi`，并从 probe 中删除显式 yield。
CPU1 helper 在首次 CPU0 getcpu 后向 CPU0 发送生产 RESCHEDULE；用例同时要求 CPU0
安全点消费计数增长、同一 TCB 在 CPU1 返回用户态、getcpu 观察 0→1、两次 affinity
仍为 `0b11`，以及 helper/user TCB 都完成回收。只看到 21/21 或 `last_cpu=1` 不足以
证明远端 IPI 是切换原因。
B34 再把该项改名为 `smp::user_task_reschedules_and_sets_affinity`。probe 到达 CPU1 后调用
raw `sched_setaffinity(0, 8, bit0)`；syscall 返回后必须由 getcpu 直接观察到 CPU0，再由
getaffinity 读到 bit0。三段断言分别拒绝“B33 IPI 未迁移”“只改 mask 未迁移”和“迁移但
未持久发布 mask”的假阳性。CPU0 runner 的等待循环必须调用既有任务安全点，否则只开中断
只能接收 need_resched、不能按照安全点抢占模型让出 CPU；全局 zombie 队列会被 idle drain，
不得把“队列保持非空”当作任务已经退出的稳定条件。

B35 新增当前列表第 13 项 `smp::blocked_affinity_redirects_wake`，总数变为 22。用例把 kernel-only
任务先定向到 CPU1，经真实 Completion/WaitQueue 进入稳定 Blocked，并同时确认 CPU1 current
与 runqueue 已释放；CPU0 再通过生产入口把 mask 改为 bit0，随后 Completion wake。任务必须在
CPU0 恢复并退出，旧 CPU1 不得残留 owner。B34 的用户探针因此在当前列表顺延为第 21 项；
B34 历史证据中的第 20 项编号保持原样。该用例动态覆盖 manager/wake 协议，远程 raw syscall
的 TID 查找、权限和用户指针路径仍以 B34 用户 probe 与源码审计组合验收。

B36 再插入第 14 项 `smp::queued_affinity_moves_between_runqueues`，总数变为 23。CPU1 holder
开放中断以响应 kernel-stack TLB 同步，但不经过调度安全点；第二个任务因此稳定保持
`Queued(1)`。用例先把 mask 扩为 bit0|bit1，证明 owner 仍合法时不会搬队；再收紧为 bit0，
核对源/目标队列长度、mask、`Queued(0)` 和最终恰好一次 CPU0 执行。B34 用户 probe 顺延为
第 22 项，terminal STOP 为第 23 项。该项直接调用生产 manager/runqueue 入口，尚未从用户态
并发发起两个远程 TID syscall，也不覆盖远程 Running/Blocking 停止协议。

### 目录结构

```
os/src/kernel_tests/
├── mod.rs            # 注册所有测试组，run_from_bootargs() 入口
├── runner.rs         # TAP 输出、timeout/repeat/failfast
├── waitqueue.rs      # wake_before_wait_should_not_sleep 等
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

永久停止 AP、关机或不可逆破坏全局状态的用例必须用
`KernelTest::terminal(name, func)` 注册。runner 会先执行所有选中组的普通
测试及其 repeat，最后才执行 terminal 集合；terminal 不参与 repeat。
因此 `KTEST=all` 不会因 SMP STOP 提前破坏后续 MM/FS 测试，
`KREPEAT>1` 也不会尝试再次唤醒已经停止的 AP。

B22 的 SMP 组在 `KREPEAT=2` 时为 29 项：14 个普通用例各执行两轮，STOP terminal
只执行一次。除既有 online/idle/IPI/timer/current owner 外，还必须看到两轮
`configured_cpus_enter_scheduler`、`remote_kernel_tasks_run_on_target_cpus` 和
`blocked_kernel_tasks_wake_on_last_cpu` 通过。后者让每个 AP 任务进入真实
Completion/WaitQueue，CPU0 在确认所有任务均为 `Blocked` 且离开 current/runqueue 后
一次批量 complete；恢复任务必须仍由原 AP 的 `Running(cpu)` current 唯一拥有并正常退出。

`kernel_stack_reclaim_waits_for_shootdown` 每轮创建 129 个 CPU1 kernel task，强制越过
128 项 stack mapping cache；它必须观察所有 AP 的 TLB ack、确认 TCB 强引用消失，并以
第二轮任务验证回收 slot 的重新映射和执行。shootdown 等待会临时开中断，因此 ktest 在
退出该用例前显式经过生产 timer 安全点，避免把已静默的 one-shot 泄漏给下一轮 timer 测试。

`user_tlb_full_flush_reaches_online_cpus` 直接调用生产 `synchronize_user_tlb_mask()`，要求
每颗在线 AP 的独立 user-TLB ack sequence 增长。它验收 reason/mailbox、架构本地全用户
失效入口和 ack 等待闭环；用例末尾同样经过 timer 安全点。该用例没有修改真实用户 PTE，
因此不能用于声称 generation race、stale translation、ack 前 frame 不复用或用户迁移
已经完成；这些属于 B23 的 MM focused 测试。

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

TAP 兼容标准测试消费者。失败时 YAML block 包含 `reason` 和 `elapsed_ms`。

### 当前测试清单 (15 个)

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
| `waitqueue::basic_queue_ops` | `waitqueue.rs` | 新建队列 → is_empty → compact_stale → is_empty |
| `waitqueue::wake_all_on_empty` | `waitqueue.rs` | 空队列 `wake_all()` 返回 0 |
| `ext4::memblk_read_write` | `ext4.rs` | `TestMemBlock` BlockDevice 读写正确性 |
| `ext4::memblk_isolation` | `ext4.rs` | 两个独立 `TestMemBlock` 实例的数据不互泄露 |
| `ext4::open_unformatted_returns_err` | `ext4.rs` | 未格式化设备上 `open_ext4rs` 返回错误（不 panic） |
| `ext4::lw_path_isolation` | `ext4.rs` | lwext4 `lw_path()` 路径翻译的实例隔离语义 |

**规划中**（需要内核线程 spawn API 或格式化块设备）：
- `waitqueue::wake_once`, `wake_all` — 多任务唤醒
- `sched::spawn_and_yield` — 创建线程 → yield → 验证运行
- `timer::sleep_returns` — 真正阻塞等待 deadline
- `fs::tmpfs_create_write_read_unlink` — VFS 基础路径
- `pagecache::basic_insert_lookup_evict` — 页缓存操作

### 执行

```bash
# 跑全部 L3 测试
make rv64-ktest

# 指定测试组
make rv64-ktest KTEST=waitqueue

# 压力测试（重复 1000 次抓偶发 bug）
make rv64-ktest KTEST=waitqueue KREPEAT=1000

# 打开 trace
make rv64-ktest KTEST=sched KTRACE=waitqueue,sched

# 跨架构对照
make la64-ktest KTEST=all
```

Makefile 在编译时通过 `MANGO_CMDLINE` 环境变量注入 bootargs，内核通过 `option_env!("MANGO_CMDLINE")` 读取。Ktest 模式的 QEMU 启动不挂载磁盘镜像，仅需内核二进制。

### 添加新的 L3 测试

1. 在 `os/src/kernel_tests/` 下创建 `my_subsystem.rs`
2. 实现 `pub fn tests() -> Vec<KernelTest>`
3. 在 `mod.rs` 中注册：`#[path = "my_subsystem.rs"] mod kt_my;` 并在 `all_tests()` 中添加条目
4. 确保测试函数 compute-bounded（不无限阻塞），失败路径返回 `Err("reason")`

---

## Bootargs 机制

### 格式

```text
mango.mode=normal|ktest|regression
mango.test=all|waitqueue|sched|timer|mm|fs|pagecache|block|arch|basic
mango.test.repeat=100
mango.test.timeout_ms=5000
mango.test.failfast=1
mango.trace=waitqueue,sched,timer
mango.init=/bin/sh
mango.root=/dev/vda
```

解析规则：空格分隔、`key=value`、逗号列表值、无值 flag、无引号/转义。

### 实现

纯解析逻辑在 `libs/mango-kernel-core/src/bootargs.rs`（L1 可测）。内核侧 wrapper (`os/src/bootargs.rs`) 提供 `load()` 函数，当前通过编译期 `env!("MANGO_CMDLINE")` 获取命令行。后续 DTB `/chosen/bootargs` 或 EFI 支持后，运行时源优先，编译期常量作为 fallback。

### HAL/Arch 分层

| 层 | 职责 |
|----|------|
| HAL/arch | 提供事实：如何拿到 cmdline、shutdown、timer、console |
| 通用内核 | 策略：解析 `mango.mode`、选择测试、控制 repeat/timeout/trace |

同一串 `mango.mode=ktest mango.test=waitqueue` 在 rv64 和 la64 上语义一致。

---

## L4 — 用户态 Regression Test

### 规范

每遇到一个 LTP/lmbench/手写测试暴露的 bug，沉淀一个最小用户态复现程序，放入 `user/src/bin/regression/` 目录。运行入口：

```bash
make regression        # rv64 回归测试
make rv64-regression   # 同上（显式架构）
make la64-regression   # la64 架构
```

### 运行机制

1. `make regression` → 编译所有用户程序（含 `regression` 二进制） → 构建文件系统镜像 → 构建内核 → 通过 `debugfs` 将 `regression_test.conf`（`mode=regression`）注入 rootfs → 启动 QEMU → 解析串口输出中的 `[L4 REGRESSION PASSED/FAILED]` 字样
2. initproc 启动后读取 `/os_test.conf`，识别 `mode=regression`，跳过 `prepare_symlink` 等环境准备，直接 fork + exec `/regression`
3. `/regression` 输出 TAP 格式结果（`ok N name` / `not ok N name`）、累加 pass/fail 计数，exit 0=全部通过 / 非零=有失败
4. initproc 通过 `exit_code_from_waitpid_status()` 获取子进程退出码，打印 `[L4 REGRESSION PASSED]` 或 `[L4 REGRESSION FAILED]`，然后 `shutdown()`

### 当前覆盖 (4 个用例)

---

## L5 — 官方集成测试

### 测试组

由 `os_test.conf` 的 `mask` 字段控制（12-bit）：

| 位 | 掩码 | 测试组 | 用途 |
|----|------|--------|------|
| 0 | `0x001` | basic | 冒烟 |
| 1 | `0x002` | busybox | 基础命令 |
| 2 | `0x004` | lua | 脚本解释器 |
| 3 | `0x008` | libctest | C 库测试 |
| 4 | `0x010` | iozone | 文件 I/O 性能 |
| 5 | `0x020` | unixbench | 系统基准 |
| 6 | `0x040` | iperf | 网络吞吐 |
| 7 | `0x080` | libcbench | C 库基准 |
| 8 | `0x100` | lmbench | 微基准 |
| 9 | `0x200` | netperf | 网络性能 |
| 10 | `0x400` | cyclictest | 实时延迟 |
| 11 | `0x800` | LTP | Linux 兼容性 |

常用 mask：`0x001` (basic)、`0x003` (basic+busybox)、`0x800` (LTP)、`0xFFF` (全量)。

### 执行

```bash
# 注入测试配置
make -C os conf-inject CONF_ARCH=rv64 CONF_FILE=../os_test.conf

# QEMU 运行
cd os && make rv64-run

# 全量自动化
python3 scripts/run_full_test.py
```

### SMP 8 核初赛非回归门禁

SMP 中改变普通用户任务执行路径的 T3 节点，以及 Phase/合并候选，必须在 Docker 内严格
串行执行 RV64、LA64 的 normal `CORE_NUM=8` + `mask=0x003`。四组 START/END、脚本
`exit_code=0`、`online_mask=0xff`、无 panic/timeout/source drift 是硬条件；judge 还必须
识别 314 个计分点，且得分和精确失败集合相对人工接受基线不退化。

当前 raw 参考为 RV64 312/314、LA64 305/314；semantic 最低分为 RV64 312/314、
LA64 308/314。两者差异只来自执行规范中对官方 `test_pipe` 多 write 输出交错的严格块级
归一化，raw judge 分数必须原样报告。不能只比较总分：同分但失败项换位也视为未通过；
更好结果需稳定证据和人工确认后才向上 ratchet，任何失败都不能反向降低基线。纯文档/注释
可复用同一代码快照的新鲜结果，局部 helper 按风险使用 focused test。完整触发条件、归一化
前提、允许失败集合和证据边界见
[SMP Agent 执行规范](../10_plan/smp-agent-execution-spec.md#82-双架构-8-核初赛非回归门禁)。

B28/B29/B30/B31/B32/B33/B34/B35/B36 这类改变用户 trap CPU、current owner、用户可见 CPU 编号、
affinity 查询或入队允许集的节点，先执行双架构初赛门禁，再在最终小范围收敛后重复
双架构 SMP focused。B29/B30 验收必须在 TAP 中直接看到
`smp::user_task_migrates_on_yield`，不能只依据 21/21 总数；还要区分首轮 RED 中的
shootdown missing CPU 与发起 CPU。exit 是非返回 trap，日志/文档不得把它描述为第三次
完整往返。B30 还要求 probe 自身检查 getcpu 的 `0 -> 1`，并通过被回收进程的 exit status
传递结果；仅由内核测试线程读取 `last_cpu == 1` 不能证明 syscall 没有继续固定返回 0。
B31 另外要求 TAP 中的第 11/12/20 项均明确 PASS；这三项是正向路径证据，
不得写成“已穷举所有违规 placement”。最终判定还要结合三个 runqueue 入口的
fail-stop 源码审计与冻结源码指纹。B32 还要求第 20 项进程 exit status 间接确认两次
raw 返回值和 mask 自检；严格 TID 查找必须单独检查 syscall 没有使用 PID fallback helper。
B33 起第 20 项名称变为 `smp::user_task_reschedules_from_ipi`；必须同时核对 helper 发送、
CPU0 消费计数、probe 自身 getcpu/affinity/exit 和最终 Weak 回收。旧 B29—B32 证据中的
历史测试名保持不变，不能倒写成当时已经完成 IPI 驱动安全点。
B34 起第 20 项名称变为 `smp::user_task_reschedules_and_sets_affinity`；除 B33 证据外，还必须
核对 setaffinity 后 getcpu=0、getaffinity=bit0、最终 `last_cpu=0`。远程 TID、短/长 mask
错误路径和 Queued/Blocked 写侧未被该正向 probe 覆盖，必须在报告中保留边界。
B35 插入新的第 13 项后，当前列表中的 B34 probe 顺延为第 21 项；验收必须同时看到旧
`blocked_kernel_tasks_wake_on_last_cpu`、新 `blocked_affinity_redirects_wake`、B34 probe 与
终态 STOP 全部 PASS。新用例证明稳定 Blocked 的 mask 会改变真实 wake 目标，但没有从用户态
直接发起远程 TID syscall。
B36 插入第 14 项后，B34 probe/STOP 分别顺延为第 22/23 项；验收必须看到
`queued_affinity_moves_between_runqueues` 在双架构直接 PASS，并核对 holder 释放后源队列无残留、
subject 只在 CPU0 执行一次。该证据只闭合稳定 Queued 写侧；Running/Blocking 必须继续标为
未支持，不能用 23/23 外推完整远程 affinity。

### Bug 下沉流程

L5 发现 bug 后：先尝试写 L4 regression → 如涉及内核机制，进一步下沉为 L3 → 如根因在纯逻辑，提取 L1 用例。

---

## Makefile 命令速查

> 所有命令在 **Docker 容器内**的项目根目录 (`/app`) 执行。
> `make docker` 进入容器。

```bash
# 根目录评测构建，按需 setup/preflight，首次容器可能联网
make all
# 直接 OS、用户态或架构目标前运行，只读，不下载/安装
make toolchain-preflight
# 手动/direct workflow 的显式准备入口
make toolchain-setup

# L0 — 静态检查
make check-fast

# L1 — 纯逻辑单元测试（秒级，host 跑，不需要 QEMU）
make unittest                        # 等价于 cargo test -p mango-kernel-core

# L3 — 内核自检（分钟级，QEMU 内跑）
make -C os rv64-ktest                # rv64 全部 L3
make -C os rv64-ktest KTEST=waitqueue KREPEAT=100

# L4 — 用户态回归
make regression

# 一键扫 bug
make bugscan                         # unittest + L3 ktest
```

> ⚠️ **常见错误**：不要在 `os/` 或 `user/` 目录下跑 `cargo test`——它们是 `#![no_std]` 裸机 crate，host 上无法编译。L1 测试只能用 `make unittest` 或在根目录 `cargo test -p mango-kernel-core`。

---

## 跨架构定位策略

| 现象 | 优先怀疑 |
|------|----------|
| RV **和** LA 的 L3 都挂 | 通用 waitqueue/scheduler/VFS 逻辑 |
| 只有 LA 挂 | LA arch 层、timer、中断、上下文切换、原子操作、TLB/CSR |
| 只有 RV 挂 | RV arch 层、SBI、timer interrupt、trap、satp/page table |
| L3 都过，L4 regression 挂 | syscall、VFS、fd table、用户态 ABI、copyin/copyout |
| L4 都过，L5 挂 | 边界语义、特殊文件、procfs/devfs、权限、资源限制、脚本假设 |

---

## 已知限制

| 限制 | 影响 | 计划 |
|------|------|------|
| L3 timeout 是 advisory-only | 无法中断挂死测试 | Phase 2 添加 watchdog timer |
| 缺少内核线程 spawn API | wake_once/wake_all/spawn_and_yield 暂缺 | Phase 2 实现 |
| bootargs 仅编译期常量 | 真板子需要重新编译 | DTB/EFI 支持后改为运行时优先 |
| L4 已实现，L2 未实现 | 暂无属性测试和模型测试 | Phase 3 |

---

## 参考

| 项目 | 借鉴点 |
|------|--------|
| Tock OS | in-kernel test 与 cargo test 分层 |
| Rust-for-Linux | KUnit 集成、`#[test]` 风格测试 |
| Theseus OS | test application crate 组织方式 |
| phil-opp (Writing an OS in Rust) | no_std 自定义 test runner、QEMU 退出码 |
| zCore / rCore | 测试命令统一入口、rootfs 测试组织 |
| DragonOS | Rust 内核工程结构、HAL/arch 分层 |
