use isomorphic_drivers::block;

// 放在 net/utils.rs 或 syscall/net.rs 顶部
use crate::drivers::NET_DEVICE;
use crate::net::config::NET_INTERFACE;
use crate::task::manager::WaitQueue;
use crate::task::{block_current_and_run_next, current_task, suspend_current_and_run_next};
use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use spin::Mutex;

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

/// 暂时先保留应急用，应该尽量不去调用
pub fn wait_io<T: Into<isize>>(
    mut f: impl FnMut() -> Result<T, SyscallErr>,
    nonblock: bool,
) -> isize {
    wait_io_core(
        || {
            if NET_DEVICE.lock().is_some() {
                NET_INTERFACE.poll();
            }
            match f() {
                Ok(v) => v.into(),
                Err(e) => -(e as isize),
            }
        },
        nonblock,
    )
}

/// 基于 WaitQueue 的事件驱动 socket I/O 阻塞等待。
/// 每次尝试前调用 NET_INTERFACE.poll()，在 EAGAIN 时将当前任务加入 wait_queue 然后真阻塞。
/// 与 wait_io 的区别：使用 block_current_and_run_next（真阻塞）而非 yield，收到 wake_one 信号后恢复。
pub fn wait_socket_io(
    mut f: impl FnMut() -> Result<isize, SyscallErr>,
    wait_queue: Option<&Mutex<WaitQueue>>,
    nonblock: bool,
) -> isize {
    loop {
        if NET_DEVICE.lock().is_some() {
            NET_INTERFACE.poll();
        }
        match f() {
            Ok(v) => return v,
            Err(SyscallErr::EAGAIN) => {
                if nonblock {
                    return -(SyscallErr::EAGAIN as isize);
                }
                // 将当前任务加入 wait_queue（加 task 时不需要持有数据锁，
                // 单核上 dispatch_udp_packets 只会在 poll() 中执行，
                // 而当前流在 try 失败后、加 queue 前不会被调度走）
                if let Some(wq) = wait_queue {
                    let task = current_task().unwrap();
                    wq.lock().add_task(Arc::downgrade(&task));
                    drop(task);
                }
                
                // 真阻塞——由 wake_one / wake_all 或定时器超时唤醒
                block_current_and_run_next();
                // 醒来后检查是否有待处理信号
                let task = current_task().unwrap();
                let mut inner = task.acquire_inner_lock();
                if !inner.sigpending.is_empty() {
                    // 如果有信号待处理，返回 EINTR
                    return -(SyscallErr::EINTR as isize);
                }
                inner.refresh_real_timer();
                drop(inner);
            }
            Err(e) => return -(e as isize),
        }
    }
}
