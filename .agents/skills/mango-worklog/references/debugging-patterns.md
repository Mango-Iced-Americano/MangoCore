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

### bind/umount 后 `/proc/mounts` 仍有 sandbox 残留
- 症状：LTP `fs_bind*` 清理阶段反复提示 `There are still mounts in the sandbox`，`umount` 看似成功但同一路径仍出现在 `/proc/mounts`
- 优先检查：子 `MountFS` 是否还能通过 `self_mountpoint` 找到父 `MountFSInode`，以及父 `mountpoints` 表是否真正删除了该 inode id
- 典型根因：挂载点 backref 只保存弱引用或 overmount 旧挂载未走统一 detach，导致 `detach_from_parent_and_cleanup()` 无法摘除父表项
- 修复模式：保留稳定 parent backref，在 detach 时 `take()` 断开引用；覆盖挂载旧节点也走完整 cleanup，避免 dentry/child mount 缓存继续持有 covered subtree

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

### heap_trace live 不回落但 PCB/TCB 正常
- 先区分真实生命周期泄漏和缓存型常驻：同时看 `zpcb/stale/tcb`、heap used、free frames、对象 owner。
- la64 需要额外检查架构特定 cache，例如 kernel stack 以 `Vec<u8>` 从 kernel heap 分配并可能被全局 cache 保留；1000 fork/futex 压力可把缓存打满，看起来像 heap leak。
- 资源报告也要一起查：`/proc/meminfo` 的 `MemAvailable` 如果只看 free frames，可能把静态预留但空闲的 kernel heap 漏掉，导致 LTP 大内存用例误判 `TCONF`。
- 修复模式：给大对象 cache 设置字节上限，保留小规模复用；对用户可见资源报告区分 `MemFree` 和估算型 `MemAvailable`。

## QEMU / 测试

### `os_test.conf` 修改不生效
- 使用 `conf-inject` 重新注入镜像（不能直接改镜像中的文件）

### QEMU 进程残留
- `pkill qemu-system` 或 `pkill qemu`

### LTP 特定用例调试
- 使用 `ltp_runner=inline` + `ltp_include=testname1,testname2` 窄范围测试
- 提交前恢复为 `ltp_runner=suite` 或 `ltp_runner=script`
