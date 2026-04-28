// 放在 net/utils.rs 或 syscall/net.rs 顶部
use crate::net::config::NET_INTERFACE;
use crate::task::{WaitQueue, block_current_and_run_next_with_lock, current_task, suspend_current_and_run_next};
use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use spin::Mutex;

/// 通用的阻塞 I/O 等待循环核心（与具体设备无关）。
/// `f` 返回 isize: >=0 表示成功字节数，<0 表示 -errno（as_errno_ret 编码）。
/// 不调用 poll，适用于文件/管道/tty 等任何 fd。
/// 非阻塞模式下遇 EAGAIN 立即返回。
pub fn wait_io_core(mut f: impl FnMut() -> isize, nonblock: bool) -> isize {
    loop {
        match f() {
            v if v >= 0 => return v,
            v if v == -(SyscallErr::EAGAIN as isize) => {
                if nonblock {
                    return v;
                }
                suspend_current_and_run_next();
                let task = current_task().unwrap();
                let mut inner = task.acquire_inner_lock();
                if !inner.sigpending.is_empty() {
                    return -(SyscallErr::EINTR as isize);
                }
                inner.refresh_real_timer();
                drop(inner);
            }
            v => return v,
        }
    }
}

// 加入等待队列的阻塞唤醒
// wait_queue: 传入的等待队列
// cond：唤醒条件
pub fn wait_io_core_with_queue(
    mut f: impl FnMut() -> isize,
    nonblock: bool,
    wait_queue: &Mutex<WaitQueue>,
    mut cond: impl FnMut() -> bool
    ) -> isize 
{
    loop {
        match f() {
            v if v >= 0 => return v,
            v if v == -(SyscallErr::EAGAIN as isize) => {
                if nonblock {
                    return v;
                }
                let task = current_task().unwrap();
                {
                    let inner = task.acquire_inner_lock();
                    if !inner.sigpending.difference(inner.sigmask).is_empty() {
                        return -(SyscallErr::EINTR as isize);
                    }
                }
                let mut wait = wait_queue.lock();
                wait.prepare_to_wait(Arc::downgrade(&task));
                // 如果已经满足条件，就不睡了，回到loop重试f()
                if cond() {
                    wait.finish_wait(&task);
                    continue;
                }
                drop(task);
                block_current_and_run_next_with_lock(wait);
                // 从wake_at_most()回来
                let task = current_task().unwrap();
                //结束等待
                wait_queue.lock().finish_wait(&task);
                let mut inner = task.acquire_inner_lock();
                if !inner.sigpending.difference(inner.sigmask).is_empty() {
                    return -(SyscallErr::EINTR as isize);
                }
                inner.refresh_real_timer();
            }
            v => return v,
        }
    }
}

/// 网络 I/O 等待循环。
/// 每次尝试前先 poll 网络栈推进状态。
/// 闭包返回 Result<T, SyscallErr>，T: Into<isize>（如 isize/usize）。
pub fn wait_io<T: Into<isize>>(
    mut f: impl FnMut() -> Result<T, SyscallErr>,
    nonblock: bool,
) -> isize {
    wait_io_core(
        || {
            NET_INTERFACE.poll();
            match f() {
                Ok(v) => v.into(),
                Err(e) => -(e as isize),
            }
        },
        nonblock,
    )
}
