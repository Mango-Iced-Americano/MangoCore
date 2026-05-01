use crate::net::config::NET_INTERFACE;
use crate::task::{
    block_current_and_run_next_with_lock, current_task, has_actionable_signal,
    suspend_current_and_run_next, wait_with_timeout, WaitQueue,
};
use crate::timer::TimeSpec;
use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use spin::Mutex;

const WAIT_IO_QUEUE_FALLBACK_MS: usize = 10;

/// 通用的阻塞 I/O 等待循环核心（与具体设备无关）。
/// `f` 返回 isize: >=0 表示成功字节数，<0 表示 -errno（as_errno_ret 编码）。
/// 不调用 poll，适用于文件/管道/tty 等任何 fd。
/// 非阻塞模式下遇 EAGAIN 立即返回。
/// 注意这个函数本质是轮询，有一定问题，等其他地方都修好应该弃用
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
                {
                    let inner = task.acquire_inner_lock();
                    if !inner.sigpending.is_empty() {
                        drop(inner);
                        if has_actionable_signal(&task) {
                            return -(SyscallErr::ERESTART as isize);
                        }
                    } else {
                        drop(inner);
                    }
                }
                task.acquire_inner_lock().refresh_real_timer();
            }
            v => return v,
        }
    }
}

/// 加入等待队列的阻塞唤醒
/// wait_queue: 传入的等待队列
/// cond：唤醒条件
///
/// # 已废弃
/// 请使用 `WaitQueue::wait_until_interruptible` 或 `WaitQueue::wait_until` 替代。
pub fn wait_io_core_with_queue(
    mut f: impl FnMut() -> isize,
    nonblock: bool,
    wait_queue: &Mutex<WaitQueue>,
    mut cond: impl FnMut() -> bool,
) -> isize {
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
                    let pending = inner.sigpending.difference(inner.sigmask);
                    if !pending.is_empty() {
                        drop(inner);
                        if has_actionable_signal(&task) {
                            return -(SyscallErr::ERESTART as isize);
                        }
                    } else {
                        drop(inner);
                    }
                }
                let mut wait = wait_queue.lock();
                wait.prepare_to_wait(Arc::downgrade(&task));
                // 如果已经满足条件，就不睡了，回到loop重试f()
                if cond() {
                    wait.finish_wait(&task);
                    continue;
                }
                // 兜底 —— Option B：仅当尚未有挂起定时器时才添加新的
                // 否则依赖已有定时器（10ms 后自动过期）。
                // 防止 log=off 时高频 accept/connect 循环撑爆 KERNEL_TIMER_QUEUE。
                if !task.wait_io_timer_pending.swap(true, Ordering::AcqRel) {
                    wait_with_timeout(
                        Arc::downgrade(&task),
                        TimeSpec::now() + TimeSpec::from_ms(WAIT_IO_QUEUE_FALLBACK_MS),
                    );
                }
                drop(task);
                block_current_and_run_next_with_lock(wait);
                // 从wake_at_most()回来
                let task = current_task().unwrap();
                //结束等待
                wait_queue.lock().finish_wait(&task);
                {
                    let inner = task.acquire_inner_lock();
                    let pending = inner.sigpending.difference(inner.sigmask);
                    if !pending.is_empty() {
                        drop(inner);
                        if has_actionable_signal(&task) {
                            return -(SyscallErr::ERESTART as isize);
                        }
                    } else {
                        drop(inner);
                    }
                }
                task.acquire_inner_lock().refresh_real_timer();
            }
            v => return v,
        }
    }
}

/// 网络 I/O 等待循环，EAGAIN 时挂入指定等待队列。
///
/// # 已废弃
/// 请使用 `WaitQueue::wait_until_interruptible`（将 `NET_INTERFACE.poll()` 放在条件闭包内）替代。
pub fn wait_io_with_queue<T: Into<isize>>(
    mut f: impl FnMut() -> Result<T, SyscallErr>,
    nonblock: bool,
    wait_queue: &Mutex<WaitQueue>,
    cond: impl FnMut() -> bool,
) -> isize {
    wait_io_core_with_queue(
        || {
            NET_INTERFACE.poll();
            match f() {
                Ok(v) => v.into(),
                Err(e) => -(e as isize),
            }
        },
        nonblock,
        wait_queue,
        cond,
    )
}

/// 暂时先保留应急用，应该尽量不去调用
///
/// # 已废弃
/// 请使用 `WaitQueue::wait_until_interruptible`（将 `NET_INTERFACE.poll()` 放在条件闭包内）替代。
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
