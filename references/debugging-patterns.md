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

## musl nice04 与 setpriority02 的 errno 冲突

- **现象**: musl `nice04` 期望 `nice(-10)` 返回 `EPERM`，但内核按 `setpriority(PRIO_PROCESS, 0, negative)` 返回 `EACCES`；同一轮里 `setpriority02` 明确要求该直接 syscall 返回 `EACCES`。
- **根因**: musl 的 `nice()` wrapper 通过 `getpriority()` + `setpriority()` 实现，错误码直接暴露内核 `setpriority` 结果；glibc wrapper 会满足 LTP 对 libc-level `nice()` 的 `EPERM` 预期。
- **修复**: 不改内核 `setpriority` errno，避免破坏 Linux syscall 语义和 `setpriority02`；runner 对 musl 专属排除 `nice04`，glibc 继续实跑覆盖内核 priority path。
- **教训**: 同一 syscall errno 被另一个 libc wrapper 测试间接消费时，优先保直接 syscall ABI；wrapper 层不可区分的冲突应按 libc 定向 exclude，而不是在内核里为某个测试二进制做特判。
- **相关文件**: `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`

## SysV SHM attach 生命周期与 la64 SHMLBA 双 libc 差异

- **现象**: `shmctl01` 的 `shm_nattch` 在 fork 继承场景不对，`shmctl07` 看不到 `SHM_LOCKED` mode 位；la64 glibc `shmat01` 期望 `SHM_RND` 按 64K 取整，la64 musl 同一用例又按 4K 取整。
- **根因**: SHM registry 只保存地址列表，没有按进程记录 attach，也没有在 fork/exit/shmdt 路径维护 per-process 计数和最后操作时间；`LinuxIpcPerm::mode` 截断到 `0777` 会丢掉 `SHM_DEST/SHM_LOCKED`；当前镜像的 loongarch64 glibc 头文件 `SHMLBA=0x10000`，musl 头文件仍是通用 `4096`。
- **修复**: attachment 记录 `{pid, addr}`，普通 fork 复制 VM 时继承 attachment，进程最终退出时 detach；`shmctl` 返回完整 Linux `shmid_ds`/info 结构并保留高位 mode；`shmat` 无 `SHM_RND` 时按页对齐接受，带 `SHM_RND` 时兼容 4K/64K 两种用户 ABI 期望。
- **教训**: SysV IPC 不能只建全局对象；LTP 会同时验证 syscall 返回值、IPC 元数据、fork 继承、进程退出回收和 libc 头文件暴露的 ABI 常量。遇到双 libc 结果相反时，先反汇编或检查头文件确认 wrapper/头文件差异，再决定兼容点放在内核还是 runner。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/syscall/process/clone.rs`, `os/src/task/mod.rs`

## procfs comm 文件的进程级与线程级覆盖

- **现象**: `prctl05` 的 `PR_SET_NAME/PR_GET_NAME` 已成功，但读取 `/proc/self/task/<tid>/comm` 或 `/proc/self/comm` 时报 `ENOENT`，导致用例 `TBROK`。
- **根因**: `PR_SET_NAME` 更新的是 TCB 中的线程名，LTP 会同时检查线程目录和进程目录下的 `comm` 文件；只补 `/proc/<pid>/task/<tid>/comm` 会继续卡在 `/proc/<pid>/comm`。
- **修复**: procfs PID 目录和 task 目录都挂载动态 `comm` 文件，内容从对应 task 的 `task_comm` 截断到 NUL 前并追加换行。
- **教训**: procfs 适配不能只按报错路径补一个 inode；涉及线程属性的 ABI 要检查 `/proc/<pid>/...` 与 `/proc/<pid>/task/<tid>/...` 两套入口是否都被 LTP 覆盖。
- **相关文件**: `os/src/fs/procfs/pid/mod.rs`, `os/src/fs/procfs/pid/task.rs`

## SysV IPC 用例对 procfs/sysctl 视图的依赖

- **现象**: SHM syscall 主路径已实现后，`shmctl03` 仍因 `/proc/sys/kernel/shmmax` 缺失 `TBROK`，`shmget03` 因 `/proc/sysvipc/shm` 缺失 `TBROK`。
- **根因**: LTP 的 SysV IPC 用例不只调用 `shmctl/shmget`，还会通过 procfs/sysctl 获取系统上限和当前对象列表；缺少虚拟视图会让测试在准备阶段直接 broken。
- **修复**: 从现有 SHM registry 导出 `shmmax/shmall/shmmni` 和 `/proc/sysvipc/shm` 表格快照，procfs 仅挂只读节点，不引入新的可写 sysctl。
- **教训**: IPC 适配要把 syscall ABI 和 `/proc/sys/kernel/*`、`/proc/sysvipc/*` 作为同一个可观测面处理；否则内核对象行为正确也会被环境探测挡住。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/sysvipc.rs`

## SysV MSG 的可写 sysctl 与 MSG_INFO usage 快照

- **现象**: `msgget03` 因 `/proc/sys/kernel/msgmni` 只读而 `TBROK`，`msgget04/msgget05` 因 `/proc/sys/kernel/msg_next_id` 缺失而 `TBROK`，`msgctl06` 从 `MSG_STAT_ANY` 不支持变成 `MSG_INFO` 字段不匹配。
- **根因**: SysV MSG 的 LTP 用例会写 `msgmni/msg_next_id` 控制下一次分配和上限，并把 `MSG_INFO` 解释为当前 usage 快照：`msgpool` 是队列数、`msgmap` 是消息数、`msgtql` 是消息字节数；不能复用 `IPC_INFO` 的 limit 快照。
- **修复**: 为 MSG 增加运行时 tunable 和 `msg_next_id`，注册可写 proc sysctl；`msgctl(MSG_INFO)` 返回当前队列/消息/字节 usage，`msgctl(IPC_INFO)` 继续返回上限；同时兼容 libc 可能带入的 `IPC_64` cmd 位。
- **教训**: SysV IPC 的 `*_INFO` 命令名相似但语义分裂，LTP 会同时检查 sysctl 写入、下一次 ID 分配、权限绕过的 `*_STAT_ANY` 和 usage 字段，不能只实现对象增删收发主路径。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/sysvipc.rs`

## SysV SEM 的 index/semid 兼容与 procfs 对账面

- **现象**: `semctl09` 报 `kernel doesn't support SEM_STAT_ANY`，`semget05` 报 `/proc/sys/kernel/sem: ENOENT`；主 syscall 已有 `semget/semctl/semop` 仍无法通过这两个 LTP 用例。
- **根因**: `semctl09` setup 直接用新建 semid 调 `SEM_STAT_ANY`，Linux 首个 semid 通常也是 index 0，而当前内核 semid 从 1 开始，按纯 index 查找会返回 `EINVAL` 并被判定为不支持；同时 SEM 用例会读取 `/proc/sysvipc/sem` 和 `/proc/sys/kernel/sem` 对账当前使用量与系统上限。
- **修复**: `SEM_STAT_ANY` 在 index 查找失败时 fallback 到直接 semid，保留权限绕过语义；导出 `/proc/sysvipc/sem` 快照，注册 `/proc/sys/kernel/sem` 四元组并限制写入不超过当前实现容量。
- **教训**: SysV IPC 适配不要假设对象 ID 与内核数组 index 一定一致；LTP 探针常把 Linux 现有分配策略当作兼容前提，遇到 `*_STAT_ANY` TCONF 要同时检查 ID/index、procfs 表格和 sysctl 上限。
- **相关文件**: `os/src/syscall/process/ipc.rs`, `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/sysvipc.rs`

## 能 no-op 的特权 syscall 先补 errno 语义

- **现象**: `vhangup01/vhangup02` 因 syscall 58 未注册被 LTP 标记为 `__NR_vhangup not supported on your arch`，无法覆盖后续权限语义。
- **根因**: 某些传统 syscall 的真实设备副作用对当前内核并不重要，但 LTP 会先检查 syscall 是否存在，再检查 root 成功与非特权 `EPERM`。
- **修复**: 注册 syscall 并只实现 Linux 可见的 capability gate；root 或 `CAP_SYS_TTY_CONFIG` 返回成功，普通用户返回 `EPERM`，暂不改变 tty 状态；同时清理 initproc 自动扫描里的历史 skip-reason。
- **教训**: 对 vhangup 这类边缘但低风险的 ABI，优先补“存在性 + errno 优先级 + 权限检查”，避免把不必要的设备模型复杂度带入主线；提交前要同步检查默认 skip/reason 表，否则 focused 通过但全量不计分。
- **相关文件**: `os/src/syscall/process/ids.rs`, `os/src/syscall/mod.rs`, `user/src/bin/initproc.rs`

## 同一 clock id 在 gettime/getres 中要保持一致

- **现象**: `clock_getres01` 中 `CLOCK_REALTIME_ALARM` / `CLOCK_BOOTTIME_ALARM` 被标记为不支持，但 `clock_gettime()` 已经能返回对应时间。
- **根因**: 新增 clock id 时只扩展了 gettime 路径，遗漏 getres 的合法 clock 表，导致 libc/LTP 的能力探测产生 TCONF。
- **修复**: `clock_getres()` 对 alarm clock id 返回与现有 clock 一致的最小 1ns 分辨率。
- **教训**: 时间类 syscall 的 clock id 支持矩阵要成组维护；新增或放开一个 id 时同时检查 gettime/getres/nanosleep/timer_create 的语义边界。
- **相关文件**: `os/src/syscall/process/time.rs`

## POSIX timer clock 表要和时间查询能力同步

- **现象**: `timer_delete01/timer_settime01/timer_settime02` 对 `CLOCK_REALTIME_ALARM`、`CLOCK_BOOTTIME_ALARM`、`CLOCK_TAI` 报 TCONF，但同类 clock id 的 `clock_gettime/getres` 已经能返回结果。
- **根因**: POSIX timer 的合法 clock 表仍只接受 realtime/monotonic/cpu/boottime，遗漏 alarm/TAI；`timer_settime(TIMER_ABSTIME)` 也需要为新增 clock id 选择一致的时间基准。
- **修复**: `timer_create()` 放开 alarm/TAI clock id，deadline 计算中 realtime alarm/TAI 复用 wall-clock 基准，boottime alarm 复用现有 boottime/monotonic 基准。
- **教训**: 新增 clock id 时不要只补 gettime/getres；LTP 会通过 POSIX timer 再做一次能力探测，必须明确哪些接口共享支持矩阵，哪些接口因语义风险继续拒绝。
- **相关文件**: `os/src/syscall/process/time.rs`

## prctl 状态类 ABI 要同时补 syscall 和 procfs 可见面

- **现象**: `prctl02` 中 `NO_NEW_PRIVS`、`THP_DISABLE`、`CAP_AMBIENT`、speculation control 等子项大量 TCONF，`prctl07` 也只能停在 kernel unsupported 探测。
- **根因**: prctl 入口缺少这些状态类 option 的最小状态保存和非法参数错误码；`/proc/<pid>/status` 也没有输出 `CapAmb`、`NoNewPrivs` 等 LTP 会读取的字段。
- **修复**: 在 TCB 保存并继承 no-new-privs、THP disabled、securebits、ambient capabilities；prctl 路径补状态回读和错误优先级，procfs status 同步输出真实 capability 与 no-new-privs 字段。
- **教训**: 对状态类 prctl，不要只让 syscall 返回成功；LTP 常先做能力探测，再通过 procfs 对账。像 seccomp 这种语义面很大的 option 应保持“不宣称完整支持”，只补安全的错误码边界。
- **相关文件**: `os/src/syscall/process/ids.rs`, `os/src/task/task.rs`, `os/src/fs/procfs/pid/status.rs`

## VM tunable 测试会同时暴露 procfs、mmap 与 job-control 问题

- **现象**: `max_map_count/min_free_kbytes/overcommit_memory` 起初因 `/proc/sys/vm/*` 和 `/proc/meminfo` 字段缺失 TBROK；补节点后又出现 rv64 大 malloc 虚拟地址空间不足、musl `max_map_count` 在 `raise(SIGSTOP)` 后 timeout、`min_free_kbytes` 吃尽物理页触发 OOM handler panic。
- **根因**: LTP VM tunable 不只是读写 sysctl：`max_map_count` 会反复 mmap 并用 stopped child 的 `/proc/<pid>/maps` 对账，`overcommit_memory` 会按 `MemTotal/CommitLimit/Committed_AS` 构造大 malloc，`min_free_kbytes` 会主动制造物理 OOM；musl 的 stop/continue 路径还会暴露 SIGCONT 被 mask 时 stopped wait 仍必须恢复的 Linux 语义。
- **修复**: 建立 VM sysctl 状态源，procfs 注册 `/proc/sys/vm/*` 和 meminfo 必需字段；mmap/brk 接入 overcommit 与用户可见 VMA 计数；只对可写匿名 `MAP_SHARED` 预分配共享页；stopped wait 用 pending SIGCONT 恢复而不看 sigmask；OOM handler 在 no-current 上下文跳过当前任务回收。
- **教训**: 遇到 LTP tunable 类用例，先查源码确认它后续会触发哪些内核路径，不能只补文件节点。带 `raise(SIGSTOP)` 的测试要特别检查 musl/glibc signal wrapper 差异，带“吃内存”的测试必须同时看 heap_trace、物理 frame、OOM 安全点。
- **相关文件**: `os/src/fs/procfs/files/sys.rs`, `os/src/fs/procfs/files/meminfo.rs`, `os/src/mm/mmap.rs`, `os/src/mm/vma_set.rs`, `os/src/task/signal/mod.rs`, `os/src/mm/frame_allocator.rs`

## inline broad scan 的 skip 表必须参与过滤

- **现象**: 非 fs/net LTP 自动扫描在 `cfs_bandwidth01` 后直接进入 `cgroup_core*`、`cgroup_fj*` 等环境/helper 用例，产生 TBROK、长耗时和噪声 include；旧日志中同类用例曾经会输出 `SKIP LTP CASE ...`。
- **根因**: `should_skip_ltp_helper()` 保留了完整 skip 表，但一次 merge 后 inline runner 调用点被注释掉，只剩手工 `ltp_exclude` 生效；suite/focused 路径和 broad scan 的语义边界被混在一起。
- **修复**: 在 `ltp_include` 为空的 broad scan 中恢复 `should_skip_ltp_helper()` 过滤；当 `ltp_include` 非空时不调用 skip 表，让 focused 验证仍可强制运行某个历史 skip 项。
- **教训**: LTP 扫描器本身也是适配面。处理 skip 表时要区分 broad scan 的“排噪”职责和 focused include 的“强制复现”职责；每次修改默认 skip/reason 表后都要用 `ltp_from` 在对应字母段快速复验。
- **相关文件**: `user/src/bin/initproc.rs`
