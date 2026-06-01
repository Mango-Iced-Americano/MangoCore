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
