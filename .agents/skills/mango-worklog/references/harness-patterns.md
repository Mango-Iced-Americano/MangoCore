# 经验模式库

> 跨对话可复用的 bug 根因 → 修复模式。按子系统分类。

## 路径解析

### `parse_path()` 滤除 "." 组件，"." 检测必须用原始路径字符串

- **根因**: `parse_path()` 在 `os/src/fs/mod.rs` 将 `"."` 组件（`"" | "." => {}`）忽略，导致 `vfs_lookup_parent_for_start` 等函数无法感知路径最后组件是否为 `.`
- **修复**: 在调用 `vfs_lookup_parent_for_start` 前，用原始 `path` 字符串检测最后组件是否为 `.`：
  ```rust
  let trimmed = path.trim_end_matches('/');
  if trimmed == "." || trimmed.ends_with("/.") {
      return EINVAL;
  }
  ```
- **教训**: 任何需要对 `.` 进行语义判断的地方（EINVAL for rmdir/unlink、禁止 "." 作为文件名等），都不能依赖 `parse_path()` 的输出，必须在原始路径字符串上做检查。
- **相关文件**: `os/src/fs/mod.rs` (parse_path), `os/src/syscall/fs.rs` (sys_unlinkat)

## 测试 Harness / LTP

### 长测第二轮 PID 超过 `/proc/sys/kernel/pid_max`
- **根因**: 用户可见 PID/TID 只线性增长，释放时只为 `ns_last_pid` 打标记而不进入可复用池。全量 LTP/bench 多轮创建进程后，`getpid01` 会观察到 PID 超过内核暴露的 `/proc/sys/kernel/pid_max=32768`。
- **修复**: 参考 Linux `idr_alloc_cyclic/free_pid` 和 DragonOS PID namespace 分配/释放模型：释放后记录可复用 ID，普通路径仍保持线性分配；接近 `pid_max` 高水位后再复用已释放 ID，并跳过低位保留 PID。
- **教训**: 长测日志里如果第一轮通过、第二轮 `getpid01` 或 PID 边界类用例失败，应先检查 PID allocator 的 wrap/reuse 语义，而不是调高 `/proc/sys/kernel/pid_max` 规避。
- **相关文件**: `os/src/task/pid.rs`

### LTP 账号数据库已有文件要幂等补齐
- **根因**: LTP 用例可能依赖 `/etc/passwd`、`/etc/group` 里的具体账号/组存在；如果 init 逻辑只在文件不存在时写默认内容，持久测试镜像里的旧文件不会被更新。`setfsgid03` 会从 gid 1 起调用 `getgrgid()` 查找一个存在的普通组，缺少 gid 1 组时会长时间遍历并最终被 runner 超时杀掉，表现为 137。
- **修复**: 对新增账号数据库前置条件使用幂等迁移：文件不存在时创建默认内容，文件已存在时只补缺失条目，避免覆盖镜像中已有账号配置。
- **教训**: 遇到 LTP 用例启动后没有内核断言、只在 libc/账号查询阶段超时，应先检查镜像内 `/etc/passwd`、`/etc/group`、`nsswitch.conf` 的实际内容，而不是直接把用例加入排除表。
- **相关文件**: `user/src/bin/initproc.rs`

### LTP suite/inline 结果标签不能混用
- **根因**: suite runner 通过 `/ltprunner` 逐用例等待子进程，返回码可用于 `PASS/FAIL` 标签；inline runner 直接跑 LTP 二进制时，部分用例即使 summary 有 `failed > 0` 也可能返回 0，仅凭退出码会把真实 TFAIL 误标成 PASS。
- **修复**: suite 路径按 `run_case()` 返回码输出 `PASS LTP CASE`/`FAIL LTP CASE`；inline 路径的过滤项输出 `SKIP LTP CASE`，成功退出只输出中性 `DONE LTP CASE`，真实非零退出才输出 `FAIL LTP CASE`。
- **相关文件**: `user/src/bin/ltprunner.rs`、`user/src/bin/initproc.rs`

### LTP 内部 timeout 与 runner timeout 不一致
- **根因**: suite runner 外层按架构设置 case timeout，但 LTP 二进制内部还有自己的默认 timeout。la64+heap_trace+批量窗口下，单个较慢 case 可能还没触发外层超时，就先被 LTP 内部 30s watchdog 打印 `Test timeouted, sending SIGKILL!` 并返回 `TBROK`。
- **修复**: 只在确实需要更长预算的架构上传 `LTP_TIMEOUT_MUL=2`，让 LTP 内部 timeout 与 runner 外层 `DEFAULT_CASE_TIMEOUT_SECS=60` 对齐；不要在默认 30s 的架构上传 `LTP_TIMEOUT_MUL=1`，避免不同 libc 对环境解析出现额外噪声。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP TCONF 不能计为 FAIL
- **根因**: LTP 新 API 用退出码 32 表示 `TCONF`，即测试因 libc/架构/配置前置条件不满足而跳过。suite runner 若把所有非零返回码都记为 `FAIL LTP CASE`，会把 musl 缺少 `getcontext/sethostid` 这类库能力误算成内核失败。
- **修复**: `run_case()` 返回 32 时输出 `SKIP LTP CASE` 并递增 skipped，其他非零才算 failed；输出标签也必须按返回码分支打印，不能只修计数但仍无条件打印 `FAIL LTP CASE`。
- **教训**: 扩大 LTP include 时先区分 `TFAIL/TBROK` 与 `TCONF`，否则会把可接受的环境跳过项混入内核适配清单。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP runner 外层 case timeout 要能覆盖用例自身 timeout

- **根因**: LTP 二进制内部会根据 `LTP_TIMEOUT_MUL` 放大自己的 timeout，例如 fuzzy-sync 用例 `timerfd_settime02` 会把内部 timeout 提到 3m30s；suite runner 如果仍固定 60s 外层 case timeout，会在内核语义还未失败前先杀掉测试，表现为 `TBROK Test killed`。
- **修复**: 为 suite runner 增加显式 `ltp_case_timeout_secs` 配置，focused 调试长耗时用例时让外层 timeout 大于 LTP 内部 timeout；常规配置保持默认 60s，避免全量回归被单个异常 case 拖长。
- **教训**: 遇到 LTP 提示 “try exporting LTP_TIMEOUT_MUL” 或日志显示内部 timeout 已放大时，先比较 harness 外层 timeout 和 LTP 内部 timeout，再判断是否为内核卡死。
- **相关文件**: `user/src/bin/ltprunner.rs`

### LTP libc wrapper 先于 syscall 拒绝参数
- **根因**: musl/glibc 的高层 libc wrapper 可能在进入内核前先做私有 ABI 校验；例如 musl 会把 34 号实时信号视为内部保留信号，`signal()/sighold()/sigrelse()` 直接返回 `EINVAL`，即使内核 `rt_sigaction/rt_sigprocmask` 已兼容该信号。LTP 此时表现为 libc API `TBROK`，trace 中看不到对应 syscall。
- **修复**: 对确认属于 libc wrapper 差异、且内核 ABI 已正确的普通 LTP 二进制，在 `ltp_proto_compat.so` 中补窄范围 preload wrapper，直接调用 raw syscall，并同步重新生成 rv64/la64 两份 `.so` 供内核 `.incbin` 嵌入。
- **教训**: 修这类问题前先确认失败是否发生在 syscall 之前；改完 `.so` 后必须重建内核镜像，否则 QEMU 仍会运行旧的嵌入版 preload。
- **相关文件**: `os/ltp_proto_compat.c`, `user/tools/riscv64/lib/ltp_proto_compat-rv.so`, `user/tools/loongarch64/lib/ltp_proto_compat-la.so`

### initramfs 新构建产物不能被测试盘旧二进制遮蔽
- **根因**: stage-1 init 如果优先执行 `/sdcard/initproc`，下载的测试镜像里可能保留旧 runner；即使 `make rv64-only/la64-only` 已重建 initramfs 和内嵌 `/initproc`，QEMU 仍会跑测试盘旧二进制，表现为新增配置项或 smoke 分支完全不生效。
- **修复**: initramfs 模式优先 `exec /initproc`，仅在缺失时 fallback 到 `/sdcard/initproc`；定位时可用输出字符串或二进制大小变化确认实际运行的是哪份应用。
- **教训**: 修改 `user/src/bin/initproc.rs` 后，如果 QEMU 日志仍是旧格式，先检查 stage-1 exec 顺序和镜像内同名二进制，而不是继续怀疑配置注入。
- **相关文件**: `user/src/bin/init.rs`, `user/src/bin/initproc.rs`

## 信号/进程

### 事件型 fd 定时器必须接入统一 deadline queue
- **根因**: timerfd 这类 fd 状态机如果只在周期性 timer interrupt 中扫描 registry，而 `timerfd_settime()` 不向统一 high-res timer queue 注册下一次 deadline，就会把短 timeout 退化为调度 tick 粒度；更严重时，如果扫描路径没有把“唤醒了等待者”反馈给调度器，阻塞读者可能延迟到后续调度点才运行。
- **修复**: arm/disarm timerfd 后扫描 registry 找到最早 deadline，注册一个全局 sweep timer action；用 generation 让旧 sweep 自动失效；sweep 过期后唤醒所有已到期 timerfd，返回实际 wake 数并重新计算下一次 sweep。
- **教训**: 任何“fd 可读状态由时间推进触发”的对象都不能只靠周期性兜底扫描；状态更新点必须驱动统一 deadline queue，wake 路径必须把是否唤醒任务反馈给调度器。
- **相关文件**: `os/src/fs/timerfd.rs`, `os/src/task/manager.rs`

### futex wake 与信号唤醒竞态
- **根因**: waiter 被信号或 timeout 先置为 Ready 后，仍可能暂时留在 futex waitqueue 中；随后 `FUTEX_WAKE` 移除该 waiter 时如果因状态非 Interruptible 不计数，用户态 checkpoint 会认为没有唤醒到等待者并重试到超时，而 waiter 本身又会把 waitqueue 条目被移除解释成正常 wake。
- **修复**: `WaitQueue::wake_at_most()` 对 Ready-but-still-queued waiter 也按一次成功 wake 计数；timer signal 入队时按 sigmask/sigwait mask 判断是否需要唤醒 interruptible task，减少被屏蔽信号造成的伪唤醒。
- **教训**: waitqueue 的“移出队列”和调度状态不是同一个原子状态；对 futex/checkpoint 这类计数语义，移出等待队列本身就应消耗一次 wake。
- **相关文件**: `os/src/task/manager.rs`, `os/src/task/signal/mod.rs`

### pselect/ppoll 空 fd 短 timeout 过冲
- **根因**: 无请求 fd 的纯 timeout 等待复用通用 waitqueue 睡眠会经过调度器和 timer wake；heap_trace/QEMU 下 25ms 级短睡眠可能多出数百微秒，超过 LTP `pselect01_64` 的严格阈值。
- **修复**: 对 `nfds=0`/无请求 fd 且 deadline 不超过 50ms、当前没有其他 ready task 的纯 timeout 路径，用硬件 tick 短忙等；一旦发现有其他 ready task，退回原 waitqueue 睡眠路径。
- **教训**: 微秒级计时 LTP 用例要区分“有 fd/事件等待”的阻塞语义和“纯 timeout sleep”的精度语义；短忙等必须有时长上限和 ready task 逃逸条件，避免拖慢并发场景。
- **相关文件**: `os/src/fs/poll.rs`

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

### SysV SHM 每次 shmat 分配独立匿名页
- **根因**: `shmat()` 若直接复用匿名 `MAP_SHARED` 并让每个 VMA 自行分配物理页，同一 `shmid` 的多次 attach 会得到不同 backing，写入内容无法互通；fork 继承只能共享已有 VMA，不能修复独立 attach 的共享语义。
- **修复**: 在 SysV SHM segment 级别维护 backing frames，`shmat()` 为新 VMA 映射同一组 frames；只把地址选择和 VMA 插入复用到 SHM 专用 mmap 路径，不改变普通匿名 mmap 行为。
- **教训**: LTP `shmt03/shmt04/shmt06` 这类用例要同时验证“同进程多 attach”和“fork 后读写同一 segment”；通过 fork 共享不代表独立 attach 已符合 SysV SHM 语义。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/mm/mmap.rs`

### brk 扩堆不能无条件 MAP_FIXED 覆盖外部 VMA
- **根因**: `sbrk/brk` 扩堆如果直接用 `MAP_FIXED` 映射新增范围，会先 unmap 目标区间；当 SysV SHM 等外部 VMA attach 在 break 上方时，heap 会把它覆盖掉，LTP `shmt09` 会发现本应 `ENOMEM` 的扩堆意外成功。
- **修复**: 扩堆前扫描目标范围，只允许覆盖/合并已有的私有匿名可写用户 VMA；遇到共享映射、文件映射或非可写用户映射时返回旧 break。ELF program break 初始化也应取所有 `PT_LOAD` 页尾最大值，避免初始 break 落在已有 load 段之前。
- **教训**: 不能简单把所有 overlap 都视为失败；glibc/musl 启动和历史 brk 可能已经在 heap 边界留下私有匿名 VMA。需要区分 heap 自身 VMA 与 SHM/mmap 外部阻挡，否则会修好 `shmt09` 但打坏 `brk01/brk02`。
- **相关文件**: `os/src/mm/mmap.rs`, `os/src/mm/address_space.rs`

### 无界队列导致 OOM
- **根因**: 生产者-消费者队列（`VecDeque`、`Vec` 等）无上界，消费者慢于生产者时持续堆积，底层存储重分配触发大块内存请求超出堆容量
- **症状**: 运行一段时间后 `alloc` 返回 `Err`，分配请求异常大（~96MB），远超单次正常分配
- **修复**: 在 `push`/`push_back` 前检查 `len() >= MAX_QUEUE_LEN`；超限时静默丢弃并记录 warning；上限设为命名常量便于调优
- **模式**:
  ```rust
  const MAX_QUEUE_LEN: usize = 4096;
  if queue.len() >= MAX_QUEUE_LEN {
      log::warn!("queue full, dropping");
  } else {
      queue.push_back(item);
  }
  ```
- **注意**: 上限值需权衡内存和丢包率；4096 × MTU(1500) ≈ 6MB 通常安全
- **相关文件**: `os/src/drivers/net/veth.rs`

### execve/clone 路径堆耗尽
- **根因**: Vec 扩容在裸机环境下可能 panic
- **修复**: 使用 `try_reserve` 并返回 `ENOMEM`
- **相关文件**: `os/src/syscall/process/exec.rs`

### kernel stack 溢出静默破坏 heap
- **根因**: 架构把每线程 kernel stack 直接放在 kernel heap 的 `Vec<u8>` 中时，向下增长的栈一旦越界会先写坏相邻 heap 对象，后续常表现为随机 `BTreeMap`/allocator panic，而不是在真实溢出点 fault。
- **修复**: 将 kernel stack 放到页表映射的固定 kernel VA 窗口，每个 slot 只映射实际栈页，并在增长方向保留未映射 guard page；栈映射需标记为 kernel-stack 类别，避免干扰 ELF/interpreter 临时 program 映射的 `highest_addr()`。
- **教训**: 大规模 clone/futex 压力下出现随机 heap 元数据损坏时，要优先排查 per-task kernel stack 的存放位置和溢出保护；guard page 能把“延迟随机崩溃”收敛成可定位的 kernel trap。
- **相关文件**: `os/src/hal/arch/loongarch64/kern_stack.rs`, `os/src/mm/kernel_space.rs`

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

## 修复后同步解除 LTP skip

- **根因**: inline LTP broad skip 表是人工维护的扩分保护层；内核语义已经修复后，如果旧 skip 原因不删除，后续自动 include/全量扫描仍会把可通过用例排除，表现为“focused 已 TPASS，但扩分没有增长”。
- **修复**: 每次 focused 证明某个 skip 用例在双架构 musl/glibc 均 0 failure 后，同步删除对应 `should_skip_ltp_helper()` 分支，并在 Work_Log 中记录验证来源。
- **教训**: skip 表不是事实来源，日志验证结果才是；修复内核 bug 后要回扫 `user/src/bin/initproc.rs`，避免陈旧 skip 抵消本次适配收益。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `Doc/Work_Log.md`

## 架构+libc 专属 LTP 差异收敛

- **根因**: 部分 LTP 用例实际验证的是 libc wrapper、格式化库或测试镜像行为；同一内核路径在另一个 libc 或另一个架构已经 TPASS 时，强行按失败组合改内核容易破坏正确 ABI。
- **修复**: 只在 focused 证明“失败限定为某架构+某 libc，且至少一个等价路径继续覆盖内核语义”后，加入架构+libc 专属默认 exclude；inline `initproc` 与 suite `ltprunner` 必须同步。
- **教训**: 不要扩大到全 musl/全架构，也不要在内核里伪造用户态 wrapper 行为；Work_Log 必须写清楚哪个组合仍实际运行该用例。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## SysV SHM fork 继承不只复制 VMA

- **根因**: `fork` 复制地址空间后，子进程拥有了 SHM 映射 VMA，但 SysV SHM registry 仍只记录父进程 attach；`shmctl(IPC_STAT)` 的 `shm_nattch` 少计，子进程 `shmdt()` 也会因查不到 `(pid, addr)` attachment 返回 `EINVAL`。
- **修复**: 在非 `CLONE_THREAD` 的 clone/fork 成功初始化新进程后，调用 SHM registry helper 按父 pid 复制 attachment 元数据到子 pid；线程 clone 不新增进程级 attachment。
- **教训**: fork 语义需要同时复制用户页表和进程级内核元数据。LTP 看到 “nattch 计数不对 + 子进程 detach EINVAL” 时，优先检查 registry/owner 表是否接入 clone 路径，而不是只查 VMA 映射。
- **相关文件**: `os/src/task/task.rs`, `os/src/syscall/process/ipc.rs`

## signal ucontext sigset padding 偏移

- **根因**: Linux/glibc 用户态 `ucontext_t.uc_sigmask` 对外占固定 128 字节；内核若先写较小的自定义 `UserSignalMask`，又额外固定补 128 字节 padding，会把后续 `uc_mcontext` 整体后移。SA_SIGINFO handler 读取 PC 时会落到 padding，LTP `profil01` 表现为 glibc 无法记录任何 profile bucket。
- **修复**: 将 `UserSignalMask + __pad` 的总大小约束为 128 字节，`__pad` 按 `128 - size_of::<UserSignalMask>()` 计算；signal 投递和 `sigreturn` 共用同一 `UserContext::PADDING_SIZE`，避免恢复路径偏移不一致。
- **教训**: signal frame 是 libc 可见 ABI，不能按内核内部 bitset 大小直觉拼结构；涉及 `ucontext_t`、`mcontext_t`、`sigset_t` 的偏移时，优先核对“总 ABI 保留区大小”，再判断寄存器数组内容是否需要重排。
- **相关文件**: `os/src/hal/arch/riscv/trap/context.rs`, `os/src/hal/arch/loongarch64/trap/context.rs`, `os/src/task/signal/mod.rs`, `os/src/syscall/process/signal.rs`

## POSIX mqueue libc/syscall ABI

- **根因**: POSIX API 要求用户传 `/name`，但 Linux syscall 层接收的是去掉前导 `/` 的裸 name；同时 `mq_timedsend/mq_timedreceive` 的 timeout 是 `CLOCK_REALTIME` 绝对时间，不能直接交给内核单调时间等待队列。`mq_notify(SIGEV_THREAD)` 也不是普通 signal，glibc/musl 会把 32 字节 cookie 和 netlink fd 交给内核，等待内核把 cookie 写回 netlink socket 后再触发用户 callback。
- **修复**: mqueue syscall 层按裸 name 校验并返回 Linux errno；timeout 先用 realtime now 计算 duration，再转成内核 `TimeSpec::now()` deadline；`SIGEV_SIGNAL` 用 `SI_MESGQ` siginfo 投递，`SIGEV_THREAD` 复制 32 字节 cookie 并在队列空转非空时写回 netlink recv queue，通知触发后一次性清除注册。
- **教训**: 不要直接按 POSIX libc API 形态实现内核 syscall 入参；mqueue 的 name、timeout 和 notify 都经过 libc 包装，LTP 同时覆盖 musl/glibc，必须验证双 libc。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/net/socket/mod.rs`, `os/src/net/socket/netlink/mod.rs`

## LTP inline 与 suite runner 过滤表同步

- **根因**: inline runner 的 `should_skip_ltp_helper()` 会跳过已确认的 TCONF/环境项，但 suite runner 只读取默认 exclude；同一批用例在 suite 模式下仍会实际执行，LTP 返回 32 后被 harness 打成 `FAIL LTP CASE ... : 32`，表现为“inline 全量干净，suite/云端分数低”。
- **修复**: 将稳定不支持或环境不满足的非目标项同步进 `ltprunner` 默认 exclude；修复后 suite focused 日志应显示 `skip excluded case ...` 和 `filtered=0`，而不是进入 `RUN LTP CASE`。
- **教训**: 每次维护 inline broad-skip 后都要检查 `user/src/bin/ltprunner.rs`，否则本地 focused/inline 结果无法代表 suite 评测路径；需要调试被排除项时，应临时调整配置或过滤表，验证通过后再解除。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP libc wrapper 用例不能反向改 raw syscall 语义

- **根因**: LTP `clone04` 验证的是 libc `clone()` wrapper 对 NULL child stack 返回 `EINVAL`；旧 musl wrapper 会把该非法 libc API 调用继续下传，导致用户态 trampoline 在空栈附近 SIGSEGV。内核 raw `clone(SIGCHLD, 0, ...)` 仍是 fork 兼容路径，不能为了 wrapper 用例在内核里拒绝 `stack=0`。
- **修复**: 将失败限定在旧 musl wrapper 的组合做 musl-only exclude，保留 glibc 路径继续覆盖 wrapper EINVAL 行为；内核 clone/fork 语义不做伪修。
- **教训**: 遇到 LTP metadata 标注 `musl-git`/`glibc` 的用例，先区分“libc API 合约”和“内核 syscall ABI”；只有 raw syscall ABI 错误才改内核。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/clone.rs`

## LTP clock_gettime04 与虚拟化阈值

- **根因**: `clock_gettime04` 默认按 5ms 判定连续读时间跳变，只有 `tst_is_virt()` 识别到虚拟机才放宽阈值；测试镜像缺少 `systemd-detect-virt` 时，heap_trace QEMU 下 syscall/调度抖动会被记为 `TFAIL`，扩大扫描中 glibc/musl 都可能触发。
- **修复**: 将 `clock_gettime04` 作为默认环境/性能阈值项过滤，保留 `clock_gettime01/02` 对同一 syscall 的基础语义覆盖；不要为了测试阈值虚报 `clock_getres()` 精度。
- **教训**: 时间精度类 LTP 要先看测试自己的阈值来源、虚拟化检测和 libc 组合；若要重新放开，优先优化 syscall/调度耗时或补齐测试环境检测。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/time.rs`

## LTP time namespace 配置类 TCONF

- **根因**: `clock_gettime03`、`clock_nanosleep03`、`sysinfo03` 等用例会读取 `/proc/config` 并要求 `CONFIG_TIME_NS=y`；当前内核未实现 time namespace，测试返回 `TCONF(32)`，suite runner 会把非 0 退出码记成失败。
- **修复**: 将这类配置不满足项同步加入 suite `ltprunner` 默认 exclude 与 inline broad-skip 表；相邻基础 syscall 用例仍保留实际运行覆盖。
- **教训**: LTP 返回 32 不一定是 syscall 语义失败，先看日志里的 kconfig constraint；配置类 TCONF 不应通过伪造 syscall 行为解决。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP suite 环境项要按根因过滤

- **根因**: `rt_tgsigqueueinfo01` 这类 runtest 条目可能存在但镜像未携带同名二进制，`signal06` 这类用例也可能明确限定 x86_64；它们在 suite 模式下会以 127/TCONF(32) 退出，被 harness 记为失败，但不是内核 syscall 语义问题。
- **修复**: 将缺二进制、架构限定等环境项加入 suite 默认 exclude，并确认相邻 syscall 用例仍在运行覆盖真实内核路径。
- **教训**: 看到 `command not found`、`Only test on x86_64` 先归类为环境/架构项，避免为了提分修改无关内核行为。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP userspace/fs 依赖型 TCONF 过滤

- **根因**: `eventfd06` 依赖测试镜像中的 libaio，`futex_wake04` 依赖 hugetlbfs；当前镜像或内核配置不满足时 LTP 返回 TCONF(32)，suite runner 会按非 0 退出码记成失败。
- **修复**: 将这类环境依赖项加入 suite 默认 exclude，同时保留相邻基础用例实际运行，例如 `eventfd01-05/eventfd2_*`、`futex_wake01/02`、`futex_wait*`、`futex_cmp_requeue*`。
- **教训**: eventfd/futex 名下的失败不一定代表对应 syscall 错误；先读 TCONF 原因和测试依赖，再决定是补内核能力、补镜像依赖，还是作为环境项过滤。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP suite 临时目录隔离与 times CPU accounting

- **根因**: suite 模式连续跑 glibc/musl 时共用 `/tmp`，checkpoint/futex 临时文件和状态可能在长序列中互相影响；同时旧 CPU accounting 只在 timer trap 离开时累计 `ru_stime`，普通 syscall 内核时间不可见，`times()` 再向下取整会把非零亚 tick 时间变成 0，表现为 `times03` 偶发 `tms_stime = 0` 或 la64 被外层 30s timeout 误杀。
- **修复**: initproc 给每个 libc 传独立 tmpdir；la64 suite runner 保留更宽的 60s 单例外层超时；任务调度出前结算当前内核态时间，调度入时重置内核态计时起点，回用户态/退出前补齐最后一段系统态时间；`times()` 对非零 CPU 时间向上折算 USER_HZ tick。
- **教训**: `times03` 这类用例同时覆盖 harness 速度、CPU accounting 和 tick 换算，不要直接跳过；先看 `tms_utime/stime/cutime/cstime` 哪一项异常，再区分“测试超时窗口不足”和“内核记账缺失”。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/task/task.rs`, `os/src/task/mod.rs`, `os/src/task/processor.rs`, `os/src/syscall/process/time.rs`

## SysV IPC STAT_ANY 的 full id 与 index 兼容

- **根因**: Linux `SHM_STAT/SHM_STAT_ANY`、`SEM_STAT_ANY`、`MSG_STAT_ANY` 的实现会用 `ipc_obtain_object_idr()` 按 full id 映射到底层 idr 槽位；LTP 可能先用真实 shmid/semid/msqid 探测支持情况。若内核只把入参解释成“当前表的第 n 个元素”，在 id 单调递增或删除后有空洞时会返回 `EINVAL`，表现为 `kernel doesn't support *_STAT_ANY`。
- **修复**: `*_STAT_ANY` 路径先保留现有 index 枚举兼容，再允许入参本身作为现存 IPC id；权限检查仍按命令类型执行，`SHM_STAT_ANY/SEM_STAT_ANY/MSG_STAT_ANY` 不做普通读权限拦截。
- **教训**: SysV IPC 的“index”不是简单的 `BTreeMap nth()` 抽象；遇到 `*_INFO returned valid index` 或 `doesn't support *_STAT_ANY`，先抓实际 syscall 入参，确认测试传的是 slot index 还是 full id，再改 registry 查找策略。
- **相关文件**: `os/src/syscall/process/ipc.rs`

## LTP fork pid_max/大 VMA 用例要先区分环境与内核缺陷

- **根因**: `fork13` 依赖完整 `/proc/sys/kernel/pid_max` 可写与 Linux PID wrap 语义；`fork14` 依赖至少 16TB 用户 VMA 构造 fork overflow reproducer。当前内核为避免即时 TID 复用采用单调 fresh PID/TID 分配，双架构用户地址空间也无法构造 16TB VMA，因此用例会在 setup 阶段 TBROK/TCONF，而不是暴露 fork/clone 崩溃。
- **修复**: 将这类当前环境/架构无法真实覆盖的项同步加入 suite 与 inline 默认过滤；不要为了通过 setup 写一个不生效的 `pid_max` no-op，也不要为单个 reproducer 冒险扩大整套用户 VA 布局。
- **教训**: fork 压力用例失败时先看是否真的进入 `fork()` 路径；如果日志停在 sysctl 或 mmap 构造阶段，应按测试前置条件分类，保留普通 fork/clone/vfork 用例覆盖真实生命周期路径。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/task/pid.rs`, `os/src/mm/mmap.rs`

## LTP SysV IPC 结构 ABI 前置条件

- **根因**: 部分 SysV IPC 用例会先检查测试镜像中的 libc/LTP ABI 结构布局，例如 `semctl08` 要求 `struct semid64_ds` 带 `time_high` 字段，`msgctl05` 要求 `struct msqid64_ds` 带 `time_high` 字段。若用户态头文件布局不满足，用例在进入 syscall 语义前直接 TCONF，suite runner 会按 32 记失败。
- **修复**: 对这类由测试镜像 ABI 布局决定、当前内核无法通过 syscall 行为弥补的用例同步到默认过滤表；保留相邻 `semctl09/semget05/msgctl01-04/msgctl06` 等真实 SysV IPC 用例覆盖内核路径。
- **教训**: IPC 用例报 TCONF 时先区分“用户态 ABI 结构缺字段”和“内核 stat 数据错误”；前者不应通过伪造无效内核字段解决。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/ipc.rs`

## LTP SysV IPC 压力项 runtime 前置条件

- **根因**: `msgstress01` 会大量 fork 并消耗 SysV message queue slot。heap_trace QEMU 下即使所有消息最终收齐，LTP 仍可能先打印 `Out of runtime during forking` 并以返回码 4 计失败，表现为测试预算/压力环境不满足，而不是普通 `msgsnd/msgrcv/msgctl` 语义错误。
- **修复**: 将长耗时压力项加入默认过滤表，保留普通 message queue 和 semaphore case 继续覆盖 IPC 创建、收发、stat、权限和删除路径。
- **教训**: stress 用例出现 runtime/fork budget 警告时，先看是否有真实 `TFAIL/TBROK/PANIC/OOM`；如果只是压力预算不足，应按长耗时项处理，避免为了单个 stress case 放大全量 LTP 超时风险。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP madvise 的 memcg/proc/config 前置条件

- **根因**: `madvise06/09/11` 依赖 cgroup/memcg 或内核配置探测，`madvise07` 依赖 memory-failure 配置，`madvise08` 依赖 `/proc/self/coredump_filter`。这些用例会在 setup 阶段 TCONF/TBROK，suite runner 按非 0 记失败，但同窗口的 `madvise01/02/03/05/10` 已覆盖当前支持的 madvise 语义。
- **修复**: 将这类配置/procfs 前置项加入默认过滤表，避免为了提分伪造 cgroup/procfs 文件或错误声明内核配置。
- **教训**: mm syscall 窗口里出现 TCONF/TBROK 时要先区分“madvise 行为错误”和“测试环境探测失败”；前者改 `sys_madvise`，后者过滤并保留基础 madvise case 覆盖。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/syscall/process/mm.rs`

## LTP seccomp 探测必须接真实执行路径

- **根因**: `prctl04` 先通过 `PR_GET_SECCOMP`/`PR_SET_SECCOMP` 探测支持情况；如果内核只让探测返回成功，却没有在 syscall 入口执行 strict/filter 策略，测试会从 `TCONF` 变成真实 `TFAIL`，例如 GET_SECCOMP/close/exit/fork 继承不按预期触发 `SIGKILL`/`SIGSYS`。
- **修复**: 保存 per-task seccomp mode/filter，fork/clone 继承过滤器，并在 syscall 分发入口按模式提前拦截；filter 安装时从用户态复制 cBPF 指令并做最小 verifier，避免保存用户指针或放行未知指令。
- **教训**: 看到 “kernel doesn't support PR_GET/SET_SECCOMP” 这类探测型 TCONF 时，不要只改 `PR_GET` 或 `/proc/config`。一旦宣称支持，就必须补齐用例后续会触发的真实 ABI 行为和 wait status 信号语义。
- **相关文件**: `os/src/syscall/process/ids.rs`, `os/src/syscall/mod.rs`, `os/src/task/task.rs`

## LTP syscall 用例可能依赖 /proc/sys 限额节点

- **根因**: `mq_open01` 这类 syscall 用例会先通过 `/proc/sys/fs/mqueue/queues_max` 保存/调整/恢复内核限额，再验证 syscall 的 `ENOSPC` 等边界错误码。若只实现 `mq_open/mq_send/mq_receive`，但缺失对应 sysctl 节点或写语义，用例会在 ProcFS 前置步骤 `TBROK`，表现为 `ENOENT` 或 `EPERM`，并没有真正覆盖到目标 syscall 的边界路径。
- **修复**: 将 sysctl 节点接到真实内核 limit 状态，读写路径复用同一份 getter/setter；syscall 创建和参数校验也读取该动态状态，而不是给 ProcFS 伪造只读常量。
- **教训**: 遇到 IPC/mm/process syscall 用例在 `/proc/sys/...` 处 broken，先判断它是不是目标 ABI 的配置面。若是，应实现最小可写 sysctl 并接入真实行为；不要只加只读文件，也不要把它简单归类成通用 FS 问题跳过。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/mod.rs`

## IPC namespace ID 不等于 IPC 对象隔离

- **根因**: `IpcNamespace` 只提供唯一 ID 时，`setns(CLONE_NEWIPC)`/`CLONE_NEWIPC` 可以通过 procfs namespace fd 切换成功，但 SysV IPC registry 若仍是全局查找，子进程会在新 IPC namespace 中用旧 namespace 的 `shmid` 成功 `shmat()`。
- **修复**: IPC 对象创建时记录 namespace id，`shmget/shmat/shmctl` 和 `/proc/sysvipc` snapshot 只匹配当前 namespace；`CLONE_NEWIPC` 不继承父进程 SHM attach 元数据，普通 fork 继承保持不变。
- **教训**: namespace 测例通过 `setns01` 这类 fd/errno 校验不代表隔离语义正确；遇到 `setns02`、`*_nstest` 要检查 registry 可见性，而不是只看 namespace fd 类型是否存在。
- **相关文件**: `os/src/task/ipc_namespace.rs`, `os/src/syscall/process/ipc.rs`, `os/src/task/task.rs`

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

### 临时 Arc 值上的 MutexGuard 生命周期问题

- **根因**: `current_netns()` 返回 `Arc<NetNamespace>`。链式调用 `current_netns().router.lock()` 创建一个临时 `Arc`，然后在同一语句中获取 `MutexGuard`。当 `MutexGuard` 赋值给变量时，临时 `Arc` 在语句结束时被释放，但 `MutexGuard` 仍然借用临时值 → `temporary value dropped while borrowed`
- **修复**: 将 `Arc` 绑定到变量，确保其存活时间超过 `MutexGuard`：
  ```rust
  let ns = current_netns();
  let mut router = ns.router.lock();  // OK
  ```
- **注意**: 如果 `MutexGuard` 不赋值给变量（仅用于链式调用如 `.clone()`、`.fill_default()`），临时 `Arc` 在 `MutexGuard` 被丢弃后释放，是安全的
- **相关文件**: `os/src/net/socket/netlink/route/route.rs`（lines 127, 169）

## LTP umask/create mode 用例要区分用户创建入口和内核内部文件

- **根因**: `umask()` 是进程 FS 状态，不是全局常量；`openat(O_CREAT)`、`mkdirat()`、`mknodat()` 等用户创建入口必须用当前 umask 清除权限位。若 `sys_umask()` 只返回 0 或创建路径不套掩码，`umask01` 会表现为新文件/目录模式总是原始 `mode`，且旧掩码返回值错误。
- **修复**: 在 `FsStatus` 保存 `umask`，让 fork/clone/unshare 复用既有 FS 状态复制/共享语义；只在用户态创建入口应用 `mode & ~umask`，避免影响 `memfd_create()` 等内核内部固定模式文件。
- **教训**: 创建模式类失败不要只看 inode 后端；先确认 syscall 层是否已经按 Linux ABI 处理进程级状态、旧值返回和内部对象例外。
- **相关文件**: `os/src/task/task.rs`, `os/src/syscall/fs.rs`

## LTP vma01 要同时检查 mmap hint 与 fork 继承 VMA 合并

- **根因**: `vma01` 先用 `mmap(NULL, 9*page)` 制造空洞，再用非 `MAP_FIXED` 的 `mmap(addr_hint, 3*page)` 期望落到该空洞中；fork 后子进程再在相邻地址新建匿名私有 VMA。若内核忽略可用 hint，初始映射可能落到别的空洞并和前驱合并；若 fork 继承 VMA 仍允许和子进程新 mmap 合并，则 `/proc/self/maps` 只看到单个 6 页 VMA。
- **修复**: 非 fixed mmap 在 hint 区间完整空闲时优先按 hint 放置；fork 复制到子进程的用户 VMA 标记为继承来源，并在匿名私有 lazy mmap merge 条件中排除这类 VMA。
- **教训**: VMA 类 LTP 失败不要只看 `/proc/self/maps` 输出格式；先确认 mmap 放置策略、VMA 起点以及 fork 后的合并条件。glibc/musl 的既有映射布局不同，忽略 hint 的 bug 可能只在其中一个 libc 暴露。
- **相关文件**: `os/src/mm/mmap.rs`, `os/src/mm/vma.rs`, `os/src/mm/vma_set.rs`, `os/src/mm/address_space.rs`

## la64 页级 TLB invalidate 必须携带当前 ASID

- **根因**: LoongArch `invtlb 0x5` (`INVTLB_ADDR_GFALSE_AND_ASID`) 的 `rj` 操作数是目标 ASID；若传 `$zero`，只会刷新 ASID 0 的非 global 页。用户进程使用非 0 ASID 时，COW/dirty/write 权限更新后的旧 TLB 项仍保留，表现为同一用户地址反复 Store page fault。
- **修复**: `tlb_invalidate_page()` 读取当前 ASID 并传给 `invtlb 0x5`；kernel page table 修改走单独的 global-page invalidate (`invtlb 0x6`)。
- **教训**: “PTE 已改且 full TLB flush 能修复，但 page flush 不能修复”时，应优先检查页级 flush 的 ASID/global 操作数，而不是继续放大全局 flush。
- **相关文件**: `os/src/hal/arch/loongarch64/tlb.rs`, `os/src/hal/arch/loongarch64/laflex.rs`

## COW 源帧 helper 返回克隆 Arc 时 strong_count 基准要加一

- **根因**: `cow_source_frame()` 返回的是克隆后的 `Arc<FrameTracker>`。如果 VMA 中只有一个真实 owner，函数返回后也会有 VMA entry + 本地变量两个强引用；用 `strong_count == 1` 判断唯一页会永远失败，导致本可原地恢复写权限的 COW 页不断走复制/换页路径。
- **修复**: 在该 helper 语义下把唯一 owner 判断改为 `strong_count <= 2`，并在原地恢复 PTE 写权限后执行对应架构的页级 TLB 刷新。
- **教训**: 用 `Arc::strong_count()` 判断共享/唯一时，必须把当前函数已经克隆出来的临时强引用计入阈值；否则调试现象会像 TLB/COW fault storm，但根因是引用计数基准错。
- **相关文件**: `os/src/mm/vma.rs`
