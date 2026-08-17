---
title: "L1 — 纯逻辑单元测试"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, l1, cargo-test, unit-test, mango-kernel-core]
---

# L1 — 纯逻辑单元测试

L1 是纯逻辑单元测试：把无内核依赖的确定性逻辑（解析、算术、状态转换）提取到独立库 crate，在 host 上编译运行，秒级反馈。

## 设计

L1 测的是**纯逻辑模块**——不依赖架构、不依赖全局状态、不依赖 I/O 的确定性代码。这类模块被提取到独立库 crate `libs/mango-kernel-core`，在 host 上编译和测试；内核通过 path dependency 引用同一份源码，不维护两份副本。

在 L0-L5 体系中，L1 承担**纯逻辑正确性**的验证：能在 `cargo test` 解决的不拖到 QEMU。L1 与 L2 在逻辑上分层但都走 `cargo test`：L1 测确定性逻辑（人工固定 testcase + 固定 expected），L2 测随机性质（proptest）或并发模型（自研 SCT，见 [l2-concurrency.md](l2-concurrency.md)）。

## 原理

L1 依赖 `#![no_std]` 库 crate 的 host 可测性：

- `libs/mango-kernel-core` 是标准 `#![no_std]` 库入口（`Cargo.toml` 声明 `#![no_std] lib, host-testable`）。
- 测试时 Cargo 自动注入 `std` 和 test harness，源码中的 `extern crate alloc` 在 host 测试下正常工作。
- 因此同一份源码既能被 `#![no_std]` 内核引用，又能在 host 上通过 `cargo test` 运行 `#[cfg(test)]` 用例。

> ⚠️ **不要**在 `os/` 或 `user/` 目录下直接跑 `cargo test`——它们是 `#![no_std]` 裸机 crate，host 上无法编译测试。L1 的 `cargo test` 只能在项目根目录通过 `-p mango-kernel-core` 指定纯逻辑库 crate。

### 库 crate 结构

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

## 如何启动运行

所有命令在 **Docker 容器内**的项目根目录 (`/app`) 执行：

```bash
# 直接 cargo test（秒级，host 跑，不需要 QEMU）
cargo test -p mango-kernel-core

# 等价 Makefile 入口
make unittest
```

### 当前覆盖（147 个用例）

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
