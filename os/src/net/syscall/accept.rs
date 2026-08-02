use crate::net::config::NET_INTERFACE;
use crate::net::socket::ACCEPT_WAITER_COUNT;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::task::WaitResult;
use crate::utils::error::SyscallErr;
use core::sync::atomic::Ordering;

/// 从监听 socket 接受一个连接，返回新的已连接 fd。
///
/// # Semantics
///
/// 调用 `socket.accept()` 获取新的已连接 `TcpSocket`，分配新 fd 并注册到
/// `TCP_SOCKETS` 表。若 `addr != 0`，将 peer 地址写入用户空间。
///
/// **阻塞模式**：在 `WaitQueue::wait_until_interruptible` 中循环，每次迭代
/// 调用 `socket.accept()`。遵循 harness-patterns 规则：`NET_INTERFACE.try_poll()`
/// 在循环外调用，永不放入 `WaitQueue` 条件闭包内。
/// 计数器 `ACCEPT_WAITER_COUNT` 防止无阻塞任务时的昂贵监听器扫描。
///
/// **非阻塞模式**：单次 `socket.accept()` 尝试。
///
/// # Locking
///
/// `WaitQueue` 条件闭包内不得调用 `NET_INTERFACE.poll()` —— 这在 smoltcp
/// 内部持锁时会导致死锁或活锁。
///
/// # Errors
///
/// - `-EAGAIN`：非阻塞模式下无可用连接。
/// - `-EINTR`：阻塞等待期间被信号中断，或由 SA_RESTART 自动重启。
/// - `-EMFILE`：fd 表已满。
/// - `-EINVAL`：非监听 socket。
///
/// # Linux Compatibility
///
/// 与 Linux 不同：smoltcp 不维护独立的 backlog/accept 队列，且 accept
/// 扫描是轮询驱动的无条件扫描，而非事件触发的精确唤醒。
pub fn sys_accept(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();
    let is_nonblock = match task.process.files().lock().get_file(sockfd as usize) {
        Ok(f) => f.is_nonblock(),
        Err(e) => return -(e as isize),
    };

    if let Some(wait_queue) = socket.accept_wait_queue() {
        if is_nonblock {
            match socket.accept(sockfd, addr, addrlen) {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            // Pre-poll OUTSIDE the WaitQueue closure — harness-patterns rule:
            // never put poll inside WaitQueue condition closure.
            NET_INTERFACE.try_poll();

            ACCEPT_WAITER_COUNT.fetch_add(1, Ordering::Relaxed);
            let result = loop {
                match WaitQueue::wait_until_interruptible(wait_queue, || {
                    // NO poll inside closure — only accept
                    match socket.accept(sockfd, addr, addrlen) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    }
                }) {
                    WaitResult::Ready(val) => break val,
                    WaitResult::Interrupted => {
                        break crate::task::RestartKind::RestartSys.syscall_result()
                    }
                    WaitResult::TimedOut => continue,
                }
            };
            ACCEPT_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
            result
        }
    } else {
        wait_io(
            || socket.accept(sockfd, addr, addrlen).map(|s| s as isize),
            is_nonblock,
        )
    }
}

/// accept4(fd, addr, addrlen, flags)
///
/// `flags` can include `SOCK_CLOEXEC` and/or `SOCK_NONBLOCK`.
pub fn sys_accept4(sockfd: u32, addr: usize, addrlen: usize, flags: u32) -> isize {
    const SOCK_CLOEXEC: u32 = 1 << 19;
    const SOCK_NONBLOCK: u32 = 0x800;

    crate::trace_event!(0xB040, sockfd as u64, flags as u64, 0, 0, 0, 0);

    let ret = sys_accept(sockfd, addr, addrlen);

    crate::trace_event!(0xB041, ret as u64, 0, 0, 0, 0, 0);

    if ret < 0 {
        return ret;
    }
    let new_fd = ret as usize;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    if flags & SOCK_CLOEXEC != 0 {
        let _ = fd_table.set_cloexec(new_fd, true);
    }
    if flags & SOCK_NONBLOCK != 0 {
        if let Ok(f) = fd_table.get_file(new_fd) {
            f.set_nonblock(true);
        }
    }

    ret
}
