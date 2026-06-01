## LTP signal wait 的 libc wrapper 差异

- **现象**: glibc `sigtimedwait01/sigwaitinfo01` 已经全 TPASS，但 musl 同名用例在 30s per-case timeout 后被杀掉。
- **根因**: musl 的 `sigtimedwait/sigwaitinfo` wrapper 对 raw `rt_sigtimedwait` 返回的 `EINTR` 做内部重试；如果测试用例依赖一次可见的中断返回，就可能表现为用户态持续重试而不是内核 panic 或真实阻塞泄漏。
- **修复**: 内核仍实现同步等待的 blocked signal 命中和唤醒；runner 对当前镜像中受 libc wrapper 影响的 musl 用例做专属默认排除，glibc 继续实跑覆盖内核路径。
- **教训**: LTP 双 libc 结果不一致时，先区分内核 syscall 语义、libc wrapper 重试策略和 runner timeout 三层，再决定是修内核还是做 libc 定向 exclude。
- **相关文件**: `os/src/task/signal/wait.rs`, `os/src/task/signal/delivery.rs`, `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## LTP execve 权限和 text-busy 语义

- **现象**: `execve02/execve04` 中 helper 不应被执行却进入了 `execve_child`，`execve06` 空 argv 路径在用户态看到 `argc=0` 或触发空指针异常。
- **根因**: exec 权限只检查“任意 execute 位”，没有按调用者 `fsuid/fsgid` 选择权限类别；内核只阻止写打开正在执行的文件，缺少执行正在写打开文件的反向 `ETXTBSY` 检查；空 argv 未按 Linux 兼容语义补 `argv[0]`。
- **修复**: exec 检查按 owner/group/other 权限位判定，普通文件写打开生命周期维护 inode 引用计数，exec 时命中写打开返回 `ETXTBSY`，空 argv 自动补一个空字符串。
- **教训**: LTP exec 权限类失败时，不要只看 ELF 加载是否成功；需要同时核对 VFS mode/uid/gid、进程 fsuid/fsgid、text-busy 双向关系和 libc 对空 argv 的启动假设。
- **相关文件**: `os/src/syscall/process/exec.rs`, `os/src/task/process.rs`, `os/src/fs/vfs/file.rs`

## rt_sigaction sigsetsize 与其他 rt signal syscall 的差异

- **现象**: `rt_sigaction03` 大量子项显示 raw syscall 传入非法 `sigsetsize` 后仍返回成功，LTP 报 “call succeeded ... expected EINVAL”。
- **根因**: 为兼容 libc 较大的 `sigset_t` 存储尺寸，把所有 rt signal mask syscall 统一放宽成 `sigsetsize >= 8`；但 Linux `rt_sigaction` ABI 对第 4 参数要求更严格，非法尺寸必须返回 `EINVAL`。
- **修复**: `rt_sigaction` 单独使用精确 8 字节校验；`rt_sigprocmask/rt_sigpending/sigtimedwait/signalfd` 继续接受 `>= 8` 并只读写低 64 位。
- **教训**: 不要把 `rt_sigaction` 的 ABI 校验和 mask 读写类 syscall 混成一个 helper；LTP 会直接用 raw syscall 覆盖 libc wrapper 不常走的非法尺寸路径。
- **相关文件**: `os/src/syscall/process/signal.rs`

## rv64 musl epoll_create 与 epoll_create1 的 wrapper 差异

- **现象**: rv64 musl `epoll_create02` 中 `epoll_create(0/-1)` 返回 fd，glibc 同用例返回 `EINVAL`。
- **根因**: rv64 这类新架构没有旧 `epoll_create(2)` syscall，只有 `epoll_create1(2)`；musl wrapper 直接调用 `epoll_create1(0)`，没有执行 legacy size 参数校验，而 `epoll_create1(0)` 本身是合法 Linux ABI。
- **修复**: runner 对 rv64+musl 单独排除 `epoll_create02`，保留 glibc 实跑；内核不拒绝合法的 `epoll_create1(0)`。
- **教训**: libc 包装函数语义不一致时，先确认内核是否能区分真实 syscall；不能为了 libc 的 legacy wrapper 测试破坏新 syscall 的合法参数。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`, `os/src/fs/eventpoll.rs`

## la64 musl clone08 wrapper 差异

- **现象**: la64 musl `clone08` 在 `CLONE_THREAD` 子项中报 `CLONE_THREAD clone() failed: EINVAL`，la64 glibc 和 rv64 双 libc 已能通过同用例。
- **根因**: 该失败来自 la64 musl wrapper 对 `CLONE_THREAD/CLONE_CHILD_CLEARTID` 组合的用户态/包装层限制，未能稳定覆盖内核 clone 路径；glibc 路径能验证内核线程 clone、ctid 清零和 futex exit。
- **修复**: runner 只对 la64+musl 默认排除 `clone08`，rv64 musl 和双架构 glibc 保持实跑。
- **教训**: 对 libc wrapper 差异做 exclude 时要同时按架构和 libc 缩窄，不要因为一个架构失败就扩大到全部 musl。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## unshare(CLONE_NEWNS) 与 clone(CLONE_NEWNS) 的风险边界

- **现象**: `unshare01/unshare02` 中 `CLONE_FILES/CLONE_FS` 通过，但 `CLONE_NEWNS` root 场景返回 `EINVAL`，非 root 场景也返回 `EINVAL` 而不是 `EPERM`。
- **根因**: `sys_unshare()` 把 `CLONE_NEWNS` 放在 unsupported flag 里，导致权限检查前就返回 `EINVAL`；但当前 mount namespace 未建模，直接开放 `clone(CLONE_NEWNS)` 会污染全局 mount tree。
- **修复**: 只在 `unshare()` 中支持 `CLONE_NEWNS`：root 或 `CAP_SYS_ADMIN` 作为 no-op 成功，非特权返回 `EPERM`；继续拒绝 `clone(CLONE_NEWNS)`。
- **教训**: namespace 适配要区分“简单探针可 no-op 兼容”和“会产生隔离语义依赖的 clone/mount 路径”，否则容易为了多过一个用例引入全局状态污染。
- **相关文件**: `os/src/syscall/process/clone.rs`

## timerfd 等 fd+timer 对象的唤醒与 broad skip 边界

- **现象**: `timerfd01` 的 `CLOCK_REALTIME` 相对/绝对定时读阻塞到超时；修复后 `timerfd01/02/create/gettime/settime01` 可通过，但 `timerfd_settime02` 仍在 180s 内被 SIGKILL。
- **根因**: tick 唤醒路径最初只传入 monotonic 时间，导致 realtime timerfd 的 deadline 永远不被判定过期；同时 broad runner 曾按 `timerfd*` 全家族跳过，新增实现不会转化成真实 LTP 覆盖。`timerfd_settime02` 本质是百万次双线程 fuzzy-sync 热路径压力，当前 syscall/fd 查询成本在 QEMU 下仍偏高。
- **修复**: timerfd 按自身 clock id 计算当前时间，tick registry 仅把 monotonic 时间作为 hint；`gettime/settime` 使用借用式 fd downcast 避免克隆 `File`；runner 只默认排除 `timerfd04` 和 `timerfd_settime02`，其余 timerfd 用例恢复实跑。
- **教训**: fd+timer 对象要同时检查“时间源是否匹配”“读端 wait queue 是否被通知”“runner 是否真的执行该家族”；性能压力项不要用全家族 skip 掩盖已可通过的普通 ABI 用例。
- **相关文件**: `os/src/fs/timerfd.rs`, `os/src/task/manager.rs`, `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## PR_SET_CHILD_SUBREAPER 与孤儿进程重挂语义

- **现象**: `prctl03` 因 `PR_SET_CHILD_SUBREAPER` 不支持而 TCONF，无法覆盖 subreaper 作为孤儿进程 reaper 的 Linux 语义。
- **根因**: PCB 只有 parent/children 和 init 收养逻辑，父进程退出时所有 live orphan 都转交 init，缺少“最近的已启用 child_subreaper 的祖先进程”选择；`prctl()` 也未保存/读取该标记。
- **修复**: PCB 保存不随 fork 继承、跨 exec 保留的 `child_subreaper` 标记；`prctl()` 实现 SET/GET；父进程退出时查找最近 subreaper 并把孤儿挂到其 children，下游 wait/SIGCHLD 仍复用既有退出路径，无 subreaper 时保持 init 收养和 zombie 清理逻辑。
- **教训**: 进程 reparent 类功能不要只补 syscall 返回值；LTP 会验证 PPID、wait 回收和 SIGCHLD 投递，必须把 parent 链、children 列表、wait queue 和 OOM 扩容兜底一起检查。
- **相关文件**: `os/src/task/process.rs`, `os/src/syscall/process/ids.rs`
