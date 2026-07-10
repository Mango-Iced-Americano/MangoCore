---
title: "测试体系 (Testing Framework)"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-07-10
tags: [testing, ktest, cargo-test, LTP, regression, tap]
---

# 测试体系

## 概述

MangoCore 采用五层自底向上的测试体系，从纯逻辑单元测试到内核自检、再到用户态回归测试和官方集成测试，建立完整的 bug 扫描工具链。目标是把问题定位逐步下沉——能在 `cargo test` 解决的不拖到 QEMU，能在 L3 解决的不拖到 LTP。

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
| L5 测试配置与注入 | `os_test.conf`, `os/Makefile` (`conf-inject`) |
| L5 测试脚本 | `scripts/run_full_test.py` |

## 分层总览

```
L0: 编译与静态检查
    cargo check  |  cargo fmt --check  |  cargo clippy
    → 秒级反馈，CI 第一道关卡

L1: 纯逻辑单元测试
    cargo test -p mango-kernel-core
    → 无内核依赖，host 上运行。当前覆盖：bootargs 解析器 (28 个用例)

L2: 属性测试 / 模型测试 (规划中)
    proptest 页缓存状态机  |  loom 并发 waitqueue
    → 同 L1 机制，未来引入

L3: 内核态 self-test
    mango.mode=ktest  |  QEMU 内运行  |  TAP 输出
    → 不启动用户态 init。当前覆盖：waitqueue / timer / sched / mm (11 个用例)

L4: 用户态 regression test (规划中)
    user/src/bin/regression_*.rs  |  make regression
    → 每个 bug 沉淀一个最小复现程序

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

编译器在 `os/` 目录内通过 Makefile 执行，自动处理双架构的工具链切换和 `lang_items.rs` 变体拷贝。

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
    ├── lib.rs           # extern crate alloc; pub mod bootargs;
    └── bootargs.rs      # Cmdline, BootConfig, BootMode + #[cfg(test)]
```

`lib.rs` 是标准 `#![no_std]` 库入口。测试时 Cargo 自动注入 `std` 和 test harness，源码中的 `extern crate alloc` 在 host 测试下正常工作。

### 执行

```bash
cargo test -p mango-kernel-core
```

### 当前覆盖 (28 个用例)

| 类别 | 用例 | 说明 |
|------|------|------|
| BootMode | `test_default_boot_mode_normal`, `test_ktest_mode` | 模式解析 |
| test 选择 | `test_single_test_group`, `test_multiple_test_groups_comma`, `test_all_tests` | 测试组逗号列表 |
| 数值参数 | `test_repeat_default_is_1`, `test_repeat_custom`, `test_repeat_clamped_to_min_1` | repeat 解析与钳位 |
| timeout | `test_timeout_default`, `test_timeout_custom`, `test_timeout_clamped_to_min_100` | timeout 解析与钳位 |
| bool | `test_failfast_default_false`, `test_failfast_true`, `test_failfast_true_alt` | bool 多值解析 |
| trace | `test_trace_groups` | trace group 逗号列表 |
| init/root | `test_init_override`, `test_root_override` | 路径覆写 |
| Cmdline | `test_cmdline_parse_simple`, `test_cmdline_parse_flag`, `test_cmdline_parse_multiple` | 基础解析 |
| Cmdline list | `test_cmdline_get_list`, `test_cmdline_get_list_empty_value` | 列表拆分与空值 |
| Cmdline usize | `test_cmdline_get_usize`, `test_cmdline_get_usize_invalid` | usize 解析与非法输入 |
| Cmdline bool | `test_cmdline_get_bool_variants` | bool 多值 (1/true/yes/on/0/false) |
| 边界 | `test_cmdline_empty_string`, `test_cmdline_missing_key` | 空串与缺失 key |
| 综合 | `test_complex_cmdline` | 完整 ktest 命令行 |

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
  → add_initproc()
  → [NEW] BootConfig::load()
  → [NEW] if mode == Ktest: kernel_tests::run_from_bootargs() → shutdown()
  → run_tasks()  // normal path only
```

插入点在 `add_initproc()` 之后、`run_tasks()` 之前。此时所有子系统（文件系统、网络、块设备）均已初始化完毕，测试可访问完整内核状态。

### 目录结构

```
os/src/kernel_tests/
├── mod.rs            # 注册所有测试组，run_from_bootargs() 入口
├── runner.rs         # TAP 输出、timeout/repeat/failfast
├── waitqueue.rs      # wake_before_wait_should_not_sleep 等
├── timer.rs          # tick_advances, time_spec_ops
├── sched.rs          # current_task_exists, ready_queue_has_init
└── mm.rs             # alloc_free_one_page, alloc_contiguous_pages
```

### 测试项结构

```rust
pub struct KernelTest {
    pub name: &'static str,       // "waitqueue::wake_once"
    pub func: fn() -> Result<(), &'static str>,
    pub timeout_ms: usize,        // 0 = use global default
}
```

### Runner 行为

| 特性 | 说明 |
|------|------|
| 测试选择 | 根据 `mango.test=waitqueue,sched` 过滤测试组；`all` 跑全部 |
| repeat | `mango.test.repeat=N`，每个测试重复 N 次（抓偶发 bug） |
| timeout | `mango.test.timeout_ms=N`，全局超时；测试可覆盖 |
| failfast | `mango.test.failfast=1`，遇第一个失败即停 |
| arch 诊断 | 输出 `# arch: riscv64` / `loongarch64` 用于 CI 区分 |

**限制**：当前 timeout 是 advisory-only — 在测试函数返回后检查耗时，无法中断挂死测试。需要后续添加 watchdog timer 才可实现抢占式超时。

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

### 当前测试清单 (11 个)

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

**规划中**（需要内核线程 spawn API）：
- `waitqueue::wake_once`, `wake_all` — 多任务唤醒
- `sched::spawn_and_yield` — 创建线程 → yield → 验证运行
- `timer::sleep_returns` — 真正阻塞等待 deadline
- `fs::tmpfs_create_write_read_unlink` — VFS 基础路径
- `pagecache::basic_insert_lookup_evict` — 页缓存操作
- `block::read_first_block` — 块设备读取

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

每遇到一个 LTP/lmbench/手写测试暴露的 bug，沉淀一个最小用户态复现程序。

### 目录规划

```
user/src/bin/regression_pipe_lost_wakeup.rs
user/src/bin/regression_pipe_close_read_eof.rs
user/src/bin/regression_fork_fd_table.rs
user/src/bin/regression_tmpfs_unlink_open_file.rs
user/src/bin/regression_select_100fds.rs
```

### 文件格式

每个 regression 文件头注释记录 bug 来源和修复点：

```rust
//! Regression: LTP pipe13 hang
//! Bug: reader sleeps forever after writer wake
//! Expected: process exits within 1s
//! Related subsystem: pipe / waitqueue / scheduler
//! Fix commit: <commit hash>
```

### 入口 (规划)

```bash
make regression   # 启动 MangoCore 正常模式，运行所有 regression_* 程序
```

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

### Bug 下沉流程

L5 发现 bug 后：先尝试写 L4 regression → 如涉及内核机制，进一步下沉为 L3 → 如根因在纯逻辑，提取 L1 用例。

---

## Makefile 命令速查

```bash
# L0
make check-fast                      # 编译 + 格式检查

# L1
cargo test -p mango-kernel-core      # 纯逻辑单元测试 (28 用例)

# L3
make rv64-ktest                      # rv64 全部 L3 测试
make rv64-ktest KTEST=waitqueue      # 指定测试组
make rv64-ktest KTEST=timer KREPEAT=100  # 重复 100 次
make la64-ktest KTEST=all            # la64 对照

# 复合入口 (规划)
make bugscan                         # check-fast + cargo test + ktest
make regression                      # L4 用户态回归
make official                        # L5 LTP + lmbench + iperf
```

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
| L4/L2 未实现 | 暂无回归测试和属性测试 | Phase 3 |

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
