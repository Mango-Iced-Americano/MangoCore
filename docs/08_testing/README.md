---
title: "测试体系 (Testing Framework)"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, ktest, cargo-test, LTP, regression, tap]
---

# 测试体系

## BuildStorm Linux/QEMU 对照诊断

时间受限场景下的单轮 Linux/QEMU 对照、分层采样字段和 MangoCore 归因规则见
[BuildStorm Linux/QEMU 对照诊断方案](buildstorm-linux-qemu-diagnostic.md)。该方案只运行
一次低开销诊断，不额外要求无采样干净基线。

## CI 评分门禁

`develop` 和 `main` 共用 [统一 CI 与 L5 评分](ci-scoring.md)：Docker Compose 中串行执行 RV64/LA64 QEMU，归档原始日志，并按 11 个 musl/glibc 组输出结构化评分 JSON。

## 概述

MangoCore 采用五层自底向上的测试体系，从纯逻辑单元测试到内核自检、再到用户态回归测试和官方集成测试，建立完整的 bug 扫描工具链。目标是把问题定位逐步下沉——能在 `cargo test` 解决的不拖到 QEMU，能在 L3 解决的不拖到 LTP。

## 分层文档索引

每层都有独立文档，按需深入阅读：

| 层 | 文档 | 一句话定位 |
|----|------|-----------|
| L0 | [l0-static.md](l0-static.md) | 编译与静态检查，秒级 CI 第一道关卡 |
| L1 | [l1-unit.md](l1-unit.md) | 纯逻辑单元测试，host 上跑 |
| L2 | [l2-concurrency.md](l2-concurrency.md) | 属性测试 / 并发模型测试（实现中） |
| L3 | [l3-ktest.md](l3-ktest.md) | 内核态 Self-Test，QEMU 内跑 |
| L4 | [l4-regression.md](l4-regression.md) | 用户态回归测试，最小复现程序 |
| L5 | [l5-integration.md](l5-integration.md) | 官方集成测试，最终验收 |

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
| L3 waitqueue 测试 | `os/src/kernel_tests/waitqueue.rs`, `waitqueue_{blocking,wake,interrupt}.rs` |
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

L2: 属性测试 / 并发模型测试 (已实现部分)
    自研 SCT（DFS Explorer）并发 waitqueue  |  proptest 页缓存状态机（规划）
    → 同 L1 机制，host 上系统化枚举交错、检查 invariant、重放 counterexample。详见 l2-concurrency.md

L3: 内核态 self-test
    mango.mode=ktest  |  QEMU 内运行  |  TAP 输出
    → 不启动用户态 init。WaitQueue 覆盖 one-shot 注册/唤醒、无 fallback 阻塞、多队列、信号、deadline 与压力路径。

L4: 用户态 regression test
    user/src/bin/regression_*.rs  |  make regression
    → 每个 bug 沉淀一个最小复现程序。initproc 新增 RunMode::Regression，配置文件注入 `mode=regression`，initproc fork+exec `/regression` → 打印 `[L4 REGRESSION PASSED/FAILED]` → shutdown

L5: 官方集成测试
    LTP / lmbench / iperf / libc-test / 比赛测例
    → 最终验收和性能趋势观察，通过 os_test.conf mask 控制范围
```

---

## L0 — 编译与静态检查

类型/格式/lint 检查，秒级 CI 第一道关卡。详见 [l0-static.md](l0-static.md)。

## L1 — 纯逻辑单元测试

无内核依赖的确定性逻辑，host 上跑，秒级反馈。详见 [l1-unit.md](l1-unit.md)。

## L2 — 属性测试 / 模型测试

自动生成操作序列、控制并发交错、检查 invariant、重放 counterexample。**实现中**，详见 [l2-concurrency.md](l2-concurrency.md)。

## L3 — 内核态 Self-Test

测试代码编译进内核，`mango.mode=ktest` 时在 QEMU 内运行，TAP 输出。详见 [l3-ktest.md](l3-ktest.md)。

## L4 — 用户态 Regression Test

每个 bug 沉淀一个最小用户态复现程序，initproc fork/exec 运行。详见 [l4-regression.md](l4-regression.md)。

## L5 — 官方集成测试

LTP / lmbench / iperf / libc-test / 比赛测例，最终验收和性能趋势观察。详见 [l5-integration.md](l5-integration.md)。

---

## Bug 下沉流程

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
| L4 已实现，L2 已实现（WaitQueue ≤ 2026-08-17 工作包） | 属性测试和模型测试已部分落地（见 l2-concurrency.md） | 继续扩展 Pipe / Scheduler 模型 |

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
