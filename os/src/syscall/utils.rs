// 放在 net/utils.rs 或 syscall/net.rs 顶部
use crate::net::config::NET_INTERFACE;
use crate::task::{current_task, suspend_current_and_run_next};
use crate::utils::error::SyscallErr;

/// 阻塞/非阻塞等待 I/O 操作完成
/// `nonblock` 为 true 时，遇 EAGAIN 立即返回。
pub fn wait_io<T: Into<isize>>(
    mut f: impl FnMut() -> Result<T, SyscallErr>,
    nonblock: bool,
) -> isize {
    loop {
        // 1. 先推动网络栈状态前进
        NET_INTERFACE.poll();

        // 2. 尝试执行操作
        match f() {
            Ok(v) => return v.into(),
            Err(SyscallErr::EAGAIN) => {
                if nonblock {
                    return -(SyscallErr::EAGAIN as isize);
                }
                // 3. 没有数据，让出 CPU
                suspend_current_and_run_next();
                // 4. 检查信号
                let task = current_task().unwrap();
                let mut inner = task.acquire_inner_lock();
                if !inner.sigpending.is_empty() {
                    return -(SyscallErr::EINTR as isize);
                }
                // 可选：刷新定时器，保持 itimer 准确性
                inner.refresh_real_timer();
                drop(inner);
            }
            Err(e) => return -(e as isize),
        }
    }
}
