---
title: "L0 — 编译与静态检查"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, l0, cargo-check, clippy, fmt, ci]
---

# L0 — 编译与静态检查

L0 是测试体系的第一道关卡：在编译期用类型检查、格式检查和 lint 拦截错误，秒级反馈，是 CI 的第一道防线。

## 设计

L0 测的是「代码能否编译、格式是否规范、是否有可疑的 lint 告警」。它不运行任何逻辑，只做静态分析，因此反馈最快（秒级），适合作为每次改动后的第一道门禁。

在 L0-L5 体系中，L0 承担**最底层、最廉价**的扫描：把「编译错误」「格式漂移」「lint 告警」这类问题在进入任何运行期测试（L1 单元测试、L3 内核自检、L4 回归、L5 集成）之前就拦截掉。目标是把问题定位逐步下沉——能在编译期解决的不拖到 QEMU。

## 原理

L0 依赖 Rust 工具链的静态检查能力：

- **类型检查**（`cargo check`）：只做类型与借用检查，不生成代码，比完整编译快得多。
- **格式检查**（`cargo fmt --check`）：校验代码是否符合 `rustfmt` 规范，保证全仓风格一致。
- **Lint**（`cargo clippy`）：运行 Clippy 的 lint 规则集，捕获可读性、正确性、性能方面的可疑模式。

编译器由根目录 `rust-toolchain.toml` 固定（`nightly-2026-05-10`）。根目录 `make all` 会派生 HOME 对应的 `RUSTUP_HOME` 和 `CARGO_HOME`，并在需要时执行 setup 和 preflight；直接运行 OS、用户态或架构目标只做 preflight，不自动安装 Rustup 工具链。

## 如何启动运行

所有命令在 **Docker 容器内**的项目根目录 (`/app`) 执行（`make docker` 进入）：

```bash
# L0 一键入口（类型 + 格式 + lint）
make check-fast
```

| 检查 | 命令 | 耗时 |
|------|------|------|
| 类型检查 | `cargo check` | ~15s |
| 格式检查 | `cargo fmt --check` | ~2s |
| Lint | `cargo clippy` | ~30s |

相关工具链准备：

```bash
# 根目录评测入口，按需 setup/preflight，首次容器可能联网
make all

# 直接运行 OS、用户态或架构目标前，先只读检查工具链
make toolchain-preflight

# 手动/direct workflow 的显式准备入口
make toolchain-setup
```

> ⚠️ 双架构共享根目录固定的 Rust nightly 和架构生成状态；必须分开串行构建，禁止并行执行双架构。
