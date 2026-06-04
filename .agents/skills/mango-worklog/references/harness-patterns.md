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

## epoll fd 嵌套监听语义

- **根因**: Linux 允许 epoll fd 被另一个 epoll 监听，只有自监听、环路和过深嵌套需要拒绝；一律拒绝目标 fd 为 epoll 会让 LTP `epoll_ctl04/05` 在搭建测试图时提前 `EINVAL`
- **修复**: `EPOLL_CTL_ADD` 对目标 epoll fd 做 DFS 检查，环路返回 `ELOOP`，超过兼容深度返回 `EINVAL`；同时让 `EventPollFile` 暴露读等待队列，父 epoll 可以等待子 epoll ready
- **教训**: epoll 的 `EPERM` 只适用于不支持 poll/epoll 的普通 fd，不应套用到 eventpoll fd；嵌套图必须防止递归扫描形成环
- **相关文件**: `os/src/fs/eventpoll.rs`

## pipe fcntl/sysctl 兼容语义

- **根因**: LTP 的 pipe/fcntl 用例不只看 `F_GETPIPE_SZ/F_SETPIPE_SZ` 是否存在，还依赖 `/proc/sys/fs/pipe-max-size`、`pipe-user-pages-*`、`ioctl(FIONREAD)`、`F_SETPIPE_SZ(0)` 和 capability 错误码优先级
- **修复**: 注册最小 `/proc/sys/fs/pipe-*` 节点；`F_SETPIPE_SZ(0)` 归一到一页；超过 `1<<31` 返回 `EINVAL`，无 `CAP_SYS_RESOURCE` 且超过 pipe max 返回 `EPERM`；pipe `FIONREAD` 返回 ring buffer 当前可读字节数
- **教训**: pipe 容量测试经常通过 `FIONREAD` 验证数据量，write/read 返回值正确但 ioctl 没实现也会失败；环形缓冲读写必须跨尾回绕，否则 64KiB 大块读写会被截断
- **相关文件**: `os/src/fs/dev/pipe.rs`, `os/src/fs/procfs/files/sys.rs`

## vmsplice 最小兼容路径

- **根因**: LTP `vmsplice04` 只需要 pipe 写入与阻塞/非阻塞语义； syscall 未实现会直接 TBROK，但完整 Linux 零拷贝页转移并非必要前置
- **修复**: 将用户 iovec 复制到内核临时缓冲，复用现有 pipe `File::write()` 与写等待队列；支持 `SPLICE_F_NONBLOCK` 返回 `EAGAIN`，阻塞模式等待 pipe 可写
- **教训**: 对裸机评测可先实现“语义兼容、安全复制”路径，覆盖用户可见行为，同时避免引入复杂页生命周期和新堆泄漏风险
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/syscall/syscall_id.rs`

## splice stream fd 阻塞语义

- **根因**: pipe/pty 等 stream fd 的底层 `File::read()`/`write()` 用 `EAGAIN` 表示暂不可读/写；`splice()` 如果不区分 fd 阻塞属性，会把阻塞 fd 上的临时不可用直接暴露给用户态，LTP `splice02` 中子进程先读空 pipe 时失败
- **修复**: `off_in/off_out == NULL` 的 stream 路径复用 inode read/write wait queue；只有 `SPLICE_F_NONBLOCK` 或 fd `O_NONBLOCK` 时才直接返回 `EAGAIN`，阻塞模式等待到非 `EAGAIN` 结果或被信号打断
- **教训**: `splice`/`tee`/`vmsplice` 这类零拷贝接口即使内部先做安全复制，也必须保留阻塞 fd 的等待语义；不要把底层 pipe 的内部重试信号当作最终 syscall errno
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/fs/dev/pipe.rs`

## fcntl POSIX record lock 生命周期

- **根因**: `F_SETLK/F_GETLK` 不只是保存一条整段锁记录；同一进程重复锁定/解锁重叠区间时，需要拆分旧区间、保留左右残余并合并相邻同类区间。只做覆盖删除会让 `F_GETLK` 返回错误的锁类型、起点和长度，LTP `fcntl11` 会在多个 block 中失败
- **修复**: 以 `(dev,inode,pid)` 维护进程级 advisory lock 表；设置新锁前拆分本 PID 重叠旧锁，插入后合并相邻同类区间；`F_GETLK` 忽略本 PID 锁并返回最早冲突区间
- **教训**: POSIX record lock 的释放也绑定 fd 生命周期：`close/close_range`、`dup2/dup3` 覆盖目标 fd、exec CLOEXEC 关闭和进程退出都要清理对应锁，否则后续 fork/exec/close 组合测试会出现假冲突或锁表残留
- **相关文件**: `os/src/syscall/fs.rs`, `os/src/task/mod.rs`, `os/src/task/task.rs`

## flock open file description 语义

- **根因**: `flock(2)` 锁跟 open file description 绑定，而不是单纯跟 PID 或 inode 绑定；fork 后继承的 fd 与父进程共享同一个 open-description，子进程对该 fd `LOCK_UN` 应释放父进程持有的 flock。按 PID 实现会让 LTP `flock03` 失败，按 inode 全局实现会让同一 fd 重入/解锁行为错误
- **修复**: 用 `vfs::File` 共享的 offset `Arc` 指针作为 open-description id，锁表按 `(dev,inode,description)` 维护；close/close_range/CLOEXEC/dup 覆盖/进程退出时按 description 引用计数释放最后一个引用
- **教训**: fcntl record lock 与 flock 都是文件锁，但生命周期不同：前者是进程级，后者是 open-description 级，不应共用 owner 规则
- **相关文件**: `os/src/fs/vfs/file.rs`, `os/src/syscall/fs.rs`, `os/src/task/process.rs`

## getcwd 跨挂载根路径重建

- **根因**: `absolute_path()` 反向重建路径时，挂载根 inode 没有自己在父目录中的名字；名字属于父文件系统里的挂载点 dentry。若直接在挂载点 inode 中查挂载根 inode 的 entry name，会返回 `ENOENT`，`getcwd()` 最终退回 symlink 逻辑路径，导致 LTP `getcwd03` 失败。
- **修复**: 遇到 `MountFSInode::is_mountpoint_root()` 且存在 `self_mountpoint` 时，先切换到挂载点 dentry，再继续从该 dentry 的父目录反查名称；对普通目录用 bounded parent/name hint 和 FS `get_entry_name()` fallback。
- **教训**: VFS 路径反查必须区分“挂载根 inode”和“挂载点 dentry”。`do_parent()` 适合路径解析 `..`，但 `getcwd()` 需要按 dentry/mount 树语义跨 mount boundary。
- **相关文件**: `os/src/fs/vfs/mount.rs`, `os/src/syscall/fs.rs`

## ns_last_pid 与 pidfd identity

- **根因**: LTP `pidfd_send_signal03` 会写 `/proc/sys/kernel/ns_last_pid`，强制下一次 fork 复用旧 PID，再验证旧 pidfd 不会指向新进程。若用户可见 PID/TID 分配器为了性能改成永远 fresh，且释放时不记录 released 状态，`ns_last_pid` 对低于当前水位的 PID 就无法生效。
- **修复**: 普通 `tid_alloc()` 继续单调递增，避免并发 fork/clone 早期复用；释放时只在 bitmap 标记 ID 已释放，不塞回普通 free-list；`set_ns_last_pid()` 对已释放 ID 设置 one-shot hint，由下一次 `alloc_fresh()` 消费。
- **教训**: pidfd 必须保存进程对象 identity，而不是只保存数字 PID；PID 复用只应让新的 `find_process(pid)` 找到新 PCB，旧 pidfd 仍应因旧 PCB `pid_released()` 返回 `ESRCH`。
- **相关文件**: `os/src/task/pid.rs`, `os/src/fs/pidfd.rs`

## POSIX timer overrun 饱和语义

- **根因**: LTP `timer_settime03` 覆盖 Linux CVE-2018-12896 场景：极小周期 timer 会产生超过 `i32::MAX` 的 overrun。`timer_getoverrun()` 固定返回 0，或每次内核 tick 只加 1，都会被该用例打穿。
- **修复**: 在 POSIX timer 状态中保存 overrun；`TIMER_ABSTIME` 初始时间已过期时按绝对 clock 差值计算初始 overrun；周期重装时用 `(now - deadline) / interval` 批量追赶遗漏到期次数，返回用户态前饱和到 `i32::MAX`。
- **教训**: timerfd/POSIX timer 的周期语义不能依赖调度 tick 频率逐次补偿；所有短 interval/长阻塞场景都应按真实时间差一次计算，否则既慢又不符合 Linux 边界行为。
- **相关文件**: `os/src/task/task.rs`, `os/src/task/manager.rs`, `os/src/syscall/process/time.rs`

## 默认致命信号日志区分同步 fault 与用户投递

- **根因**: wait/signal 类 LTP 用例会主动 `raise()`/`kill()` SIGILL、SIGSEGV，再用 wait status 验证默认动作。若 `do_signal()` 对默认 SIGILL/SIGSEGV 一律读取最近一次 trap cause 打印 `Exception(...) in application`，用户态显式投递信号会被误报成 `UserEnvCall`/`Syscall` 异常，自动扫描器可能把正常通过用例排除。
- **修复**: trap 路径把页错误、非法指令等同步异常转成 signal 时写入正向 `SEGV_*`/`ILL_*` `si_code`；`do_signal()` 只在 pending siginfo 表明这是同步 fault 时打印异常诊断，普通用户投递信号只走默认终止和 wait status。
- **教训**: signal 来源不能靠“当前或最近 trap cause”倒推；syscall 发出的 `kill/tgkill/raise` 与真实硬件 fault 在默认动作上都可能终止进程，但只有后者应进入内核异常日志。
- **相关文件**: `os/src/task/signal/mod.rs`, `os/src/task/task.rs`, `os/src/hal/arch/riscv/trap/mod.rs`, `os/src/hal/arch/loongarch64/trap/mod.rs`

## 网络

### 硬编码 IPv4 地址替换为 net_core 动态查询
- **根因**: 多处硬编码 `127.0.0.1` / `10.0.2.15` / `10.0.2.2`，QEMU 环境变更时需逐处修改
- **修复**: 用 `net_core::loopback_iface()` / `net_core::default_iface()` / `net_core::default_gateway()` 动态查询，`unwrap_or` 保留硬编码 IP 作为防御性回退
- **模式**:
  ```rust
  crate::net::net_core::loopback_iface()
      .and_then(|d| d.ip_addrs.first().map(|c| c.address()))
      .unwrap_or(IpAddress::v4(127, 0, 0, 1))
  ```
- **关键**: 必须保留 `unwrap_or` 回退，因为接口在 net_core 初始化前可能未注册；`unwrap()` 会导致过早调用 panic
- **相关文件**: 所有 `net/socket/inet/` 下引用 IP 的文件
