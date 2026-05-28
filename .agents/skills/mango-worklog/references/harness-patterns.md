# 经验模式库

> 跨对话可复用的 bug 根因 → 修复模式。按子系统分类。

## 信号/进程

### nanosleep 唤醒后死锁
- **根因**: 持有 `task.inner` 锁时调用 `has_actionable_signal(&task)`，后者尝试获取同一锁
- **修复**: 任何阻塞操作唤醒后检查信号时，必须先释放锁再调用 `has_actionable_signal`
- **相关文件**: `os/src/syscall/fs.rs`（nanosleep）、`os/src/fs/poll.rs`（pselect）

### 被屏蔽信号导致错误的 EINTR
- **根因**: 信号检查用了 `is_empty()` 而非 `sigpending.difference(sigmask)`，忽略了信号掩码
- **修复**: 必须用 `difference(sigmask)` 过滤被屏蔽信号
- **相关文件**: `os/src/task/signal/mod.rs`

### SA_RESETHAND 清掉 SA_SIGINFO
- **根因**: 信号投递后直接删除 action，handler 内 `sigaction(..., oldact)` 读到空 flags
- **修复**: `SA_RESETHAND` 只重置 handler 为 `SIG_DFL`，保留 flags/mask/restorer 供 oldact 查询

## 内存管理

### TLB 刷新遗漏
- **根因**: 修改 PTE 后未执行 `sfence.vma`（riscv）/ `invtlb`（la64），CPU TLB 缓存旧 PTE
- **症状**: CoW 绕过（父子写入同一物理页）、unmap 后读到残留数据
- **修复**: `unmap`、`block_and_ret_mut`、`set_pte_flags`、`revoke_write` 等所有 PTE 修改操作后必须 TLB 刷新
- **相关文件**: `os/src/mm/page_table.rs`

### MAP_SHARED 参与 CoW
- **根因**: fork 时 MAP_SHARED 页面被标记 CoW，破坏共享语义
- **修复**: MAP_SHARED 页面跳过 CoW，fork 时恢复 W 权限，缺页时只恢复 W 不做 CoW
- **相关文件**: `os/src/mm/memory_set.rs`

### execve/clone 路径堆耗尽
- **根因**: Vec 扩容在裸机环境下可能 panic
- **修复**: 使用 `try_reserve` 并返回 `ENOMEM`
- **相关文件**: `os/src/syscall/process/exec.rs`

## 文件系统

### ext4 sparse file hole 处理
- **根因**: `get_pblock_idx` 对 hole 返回垃圾物理地址
- **修复**: hole 返回 `Err`，`read_at` 填零，`write_at` 分配新块
- **相关文件**: `os/src/fs/ext4/`

### ext4 extent 搜索不验证覆盖范围
- **根因**: `binsearch_extent` 返回最近 extent 但不保证 `lblock` 在其范围内
- **修复**: 调用者必须检查 `lblock >= extent.first_block && lblock < extent.first_block + extent.len()`

### ext4 write_at 锁重入
- **根因**: 持有 `self.inode` 时调用 `get_new_page_cache()`，后者再次锁 `self.inode`，`TicketMutex` 不可重入
- **修复**: 缩短 inode 锁作用域，只 clone 已存在的 PageCache 做 invalidate

## 网络栈

### connect 永不返回 / pselect 永远挂起
- **根因**: Socket 就绪检查前缺少 `NET_INTERFACE.poll()`
- **修复**: `socket_r_ready()`/`socket_w_ready()` 中先 poll 再检查
- **相关文件**: `os/src/net/syscall/`

### 非阻塞 socket livelock
- **根因**: 紧循环 EAGAIN 阻止定时器中断
- **修复**: 非阻塞路径 `try_xxx` 前先调用 `NET_INTERFACE.try_poll()`
- **相关文件**: `os/src/net/syscall/`

## 错误码对齐（Linux 语义）

- setsockopt 未知 level → **ENOPROTOOPT(92)**，不是 EOPNOTSUPP(95)
- socketpair 非 AF_UNIX → **EPROTONOSUPPORT(93)**，不是 EAFNOSUPPORT(97)
- `Socket::alloc` 未知 domain → **EAFNOSUPPORT(97)**，不是 EINVAL(22)
- getpeername NULL addr → 必须先验证参数再检查连接状态，EFAULT 优先于 ENOTCONN
- mmap 非匿名映射的坏 fd → EBADF 优先于其他校验
- RISC-V 未对齐 addrlen → 需显式检查 `addrlen % 4 != 0`，硬件不报错
- 跨进程 VM 访问 → 先做权限检查返回 EPERM，再访问远程地址返回 EFAULT

## 调度/性能

### futex waiter 大规模场景 O(n²)
- **根因**: nice-aware scheduler 每次 `fetch_task()` 全队列扫描
- **修复**: ready 队列记录非默认 nice 数量；全 nice=0 走 FIFO fast path
- **相关文件**: `os/src/task/manager.rs`

### WaitQueue wake-all 路径性能
- **根因**: 每唤醒一个任务都扫描全局队列
- **修复**: 批量收集待唤醒任务，一次性更新 `TASK_MANAGER` 队列
