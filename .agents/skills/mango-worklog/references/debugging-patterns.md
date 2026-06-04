# 调试模式库

> 跨对话可复用的调试技巧和排查方法。

## 启动/Panic 排查

### QEMU 启动无显示
- 检查 `console::init()` 是否第一个被调用（在 `rust_main()` 中）
- 检查串口设备初始化顺序

### 内核 panic 定位
- 启动时加 `LOG=debug make rv64-run` 查看详细日志
- 使用 GDB 调试：`make rv64-debug` → `b rust_main` → `c`
- panic 输出包含 syscall 上下文、内存状态、任务信息（`panic_diag.rs`）

### 编译问题
- `cargo check` 在根目录一定失败 → 始终在 `os/` 目录用 Makefile 目标
- `Vec` 重复定义 → 检查是否同时 `use alloc::vec;` 和 `use alloc::vec::Vec;`
- lang_items 不匹配 → 编辑 `.rv` / `.la` 变体，不编辑 `lang_items.rs`

## 内存问题

### unmap 后读到旧数据
- 典型 TLB 刷新遗漏 → 检查 PTE 修改后是否有 `sfence.vma`/`invtlb`
- 用 GDB `info tlb` 查看 TLB 条目

### 物理地址异常（如 0xb0000000）
- la64: 检查 `MEMORY_SIZE` 是否匹配 DTB 中 RAM 范围
- rv64: 检查 `device_tree.rs` 中内存区域解析

### 堆耗尽
- 检查是否有 `try_reserve` 防御
- 查看 `heap_trace.rs` 的分配记录（需启用 feature）

## 网络问题

### Socket 操作阻塞不返回
- `connect` 不返回 → 检查是否使用 `try_connect` + `wait_io` 模式
- `accept`/`recvfrom` 不返回 → 检查 `wait_io` 中是否调用了 `NET_INTERFACE.poll()`

### 非阻塞 socket 测试失败
- 检查 `try_xxx` 前是否调了 `NET_INTERFACE.try_poll()`

## 信号问题

### 信号处理不生效
- 检查 sigaction 是否正确设置了 `SA_SIGINFO` 等 flags
- la64: 检查 `rt_sigaction` 的 sigsetsize 参数（libc 传 16 字节而非 8）

### 进程停止/继续状态异常
- 检查 `SIGSTOP`/`SIGCONT` 是否正确更新进程状态
- 检查父进程 wait 是否正确消费 stopped/continued 事件

## 性能问题

### la64 大量 page fault 慢
- 检查陷阱入口是否有不必要的 `invtlb`
- 检查页帧清零是否用了高效的 64-bit store 而非 byte-wise

### fork/wait 越来越慢
- 检查 TID 分配器是否有 O(n²) 查重
- 检查物理页释放是否有线性扫描 free-list

## QEMU / 测试

### `os_test.conf` 修改不生效
- 使用 `conf-inject` 重新注入镜像（不能直接改镜像中的文件）

### QEMU 进程残留
- `pkill qemu-system` 或 `pkill qemu`

### la64 编译失败（61+ errors）— 缺少 initramfs 特性

- **现象**: `rv64-kernel-build-only` 成功，但同样的 initramfs 代码在 la64 上报大量编译错误
- **根因**: la64 的 `make/la64o.mk` 使用 `--no-default-features`，而 rv64 使用默认 features（含 `initramfs`）。la64 内核中部分代码路径只在 `initramfs` 特性 gate 下才编译通过
- **修复**: la64 构建必须显式传递 `initramfs` 和 `preload_payloads`：
  ```
  cargo build --no-default-features --release --features "comp board_laqemu block_virt_pci log_off initramfs preload_payloads"
  ```
  或通过 `make -f make/la64o.mk build EXTRA_FEATURES="initramfs preload_payloads"`
- **注意**: 根 Makefile 没有 `la64-kernel-build-only` 目标；`rv64_all`/`la64_all` 通过不同的 Makefile 目标处理特性
- **相关文件**: `os/make/la64o.mk`, `os/Makefile`

### LTP 特定用例调试
- 使用 `ltp_runner=inline` + `ltp_include=testname1,testname2` 窄范围测试
- 提交前恢复为 `ltp_runner=suite` 或 `ltp_runner=script`
