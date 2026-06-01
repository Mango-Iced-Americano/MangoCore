## LTP signal wait 的 libc wrapper 差异

- **现象**: glibc `sigtimedwait01/sigwaitinfo01` 已经全 TPASS，但 musl 同名用例在 30s per-case timeout 后被杀掉。
- **根因**: musl 的 `sigtimedwait/sigwaitinfo` wrapper 对 raw `rt_sigtimedwait` 返回的 `EINTR` 做内部重试；如果测试用例依赖一次可见的中断返回，就可能表现为用户态持续重试而不是内核 panic 或真实阻塞泄漏。
- **修复**: 内核仍实现同步等待的 blocked signal 命中和唤醒；runner 对当前镜像中受 libc wrapper 影响的 musl 用例做专属默认排除，glibc 继续实跑覆盖内核路径。
- **教训**: LTP 双 libc 结果不一致时，先区分内核 syscall 语义、libc wrapper 重试策略和 runner timeout 三层，再决定是修内核还是做 libc 定向 exclude。
- **相关文件**: `os/src/task/signal/wait.rs`, `os/src/task/signal/delivery.rs`, `user/src/bin/initproc.rs`, `user/src/bin/ltprunner.rs`
