---
title: "L4 — 用户态回归测试"
category: testing
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [testing, l4, regression, user-mode, qemu]
---

# L4 — 用户态回归测试

L4 是用户态回归测试：每个 bug 沉淀一个最小用户态复现程序，在 QEMU 内通过 initproc fork/exec 运行，验证用户态可见行为。

## 设计

L4 测的是**用户态可见行为**——syscall、VFS、fd table、用户态 ABI、copyin/copyout 等。每遇到一个 LTP/lmbench/手写测试暴露的 bug，沉淀一个最小用户态复现程序，放入 `user/src/bin/regression/` 目录。

在 L0-L5 体系中，L4 承担**用户态语义正确性**的验证：L5 发现 bug 后先尝试写 L4 regression，如涉及内核机制再进一步下沉为 L3，如根因在纯逻辑再提取 L1 用例。L4 比 L3 更接近真实用户态（经过完整 ABI 和 rootfs），又比 L5 更聚焦（只跑最小复现程序，不跑完整测试套件）。

## 原理

L4 依赖 rootfs 注入 `regression_test.conf` + initproc fork/exec + `[L4 REGRESSION PASSED/FAILED]` 标记：

1. `make regression` → 编译所有用户程序（含 `regression` 二进制） → 构建文件系统镜像 → 构建内核 → 通过 `debugfs` 将 `regression_test.conf`（`mode=regression`）注入 rootfs → 启动 QEMU → 解析串口输出中的 `[L4 REGRESSION PASSED/FAILED]` 字样
2. initproc 启动后读取 `/os_test.conf`，识别 `mode=regression`，跳过 `prepare_symlink` 等环境准备，直接 fork + exec `/regression`
3. `/regression` 输出 TAP 格式结果（`ok N name` / `not ok N name`）、累加 pass/fail 计数，exit 0=全部通过 / 非零=有失败
4. initproc 通过 `exit_code_from_waitpid_status()` 获取子进程退出码，打印 `[L4 REGRESSION PASSED]` 或 `[L4 REGRESSION FAILED]`，然后 `shutdown()`

## 如何启动运行

所有命令在 **Docker 容器内**的项目根目录 (`/app`) 执行：

```bash
make regression        # rv64 回归测试
make rv64-regression   # 同上（显式架构）
make la64-regression   # la64 架构
```

### 当前覆盖（7 个用例）

| 用例 | 主要覆盖 |
|------|----------|
| `usercopy_pipe` | pipe 与用户内存复制边界 |
| `mmap_edge_cases` | mmap 边界语义 |
| `timer_realtime_jump` | realtime timer 与时钟跳变 |
| `rename_long_name` | rename 长名称 |
| `lwext4_truncate_hole` | ext4 稀疏文件截断 |
| `signalfd` | 阻塞 read 唤醒、fork 继承 fd 后的 sighand 动态绑定 |
| `clone_vm_second_slot` | CLONE_VM/vfork 的第二用户资源槽；破坏性探针固定最后执行 |
