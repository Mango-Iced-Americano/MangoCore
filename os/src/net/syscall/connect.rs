use super::common::check_addrlen;
use crate::net::config::NET_INTERFACE;
use crate::net::socket::UnixEndpoint;
use crate::net::Endpoint;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;
use alloc::format;

/// 将 socket 连接到远程地址。
///
/// # Semantics
///
/// 解析 `sockaddr` → `Endpoint`，对 `Unix::Path` 做绝对路径规范化，然后调用
/// `socket.connect()` 发起初始连接。
///
/// **阻塞模式**：若 `socket.connect()` 返回 `EAGAIN` 且有 `connect_wait_queue`，
/// 进入 `WaitQueue::wait_until_interruptible` 循环，条件闭包调用 `socket.try_connect()`
/// 检查握手状态。信号中断使用内部 restart class。
///
/// **非阻塞模式**：`connect()` 返回 `EAGAIN` 时立即返回 `-EINPROGRESS`
/// （与 Linux 语义一致，应用通过 `poll(EPOLLOUT)` 等待完成）。
/// 无 `connect_wait_queue` 的 socket 回退到无条件 yield 轮询的 `wait_io`。
///
/// # Errors
///
/// - `-EINVAL`：`addrlen` 超限。
/// - `-EINPROGRESS`：非阻塞连接尚未完成（正常，非错误）。
/// - `-EINTR`：阻塞等待期间被信号中断，或由 SA_RESTART 自动重启。
/// - 其他错误由 `socket.connect()`/`try_connect()` 产生。
///
/// # Linux Compatibility
///
/// 非阻塞 `connect` 在握手未完成时返回 `-EINPROGRESS`（而非 `-EAGAIN`），
/// 与 Linux 6.6 一致。
pub fn sys_connect(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    match check_addrlen(addrlen) {
        Ok(_) => {}
        Err(e) => return -(e as isize),
    }
    let addr_buf = crate::trans_ref!(addr, addrlen);
    let endpoint = match Endpoint::from_sockaddr(addr_buf) {
        Ok(ep) => ep,
        Err(e) => return -(e as isize),
    };
    log::info!("[sys_connect] endpoint from sockaddr: {:?}", endpoint);

    let endpoint = match endpoint {
        Endpoint::Unix(UnixEndpoint::Path(ref path)) => {
            let abs_path = if path.starts_with('/') {
                path.clone()
            } else {
                // 与 bind.rs 对齐：使用 CWD inode 的 absolute_path()
                let task = current_task().unwrap();
                let cwd_inode = task.process.fs().lock().working_inode.inode.clone();
                let cwd = cwd_inode.absolute_path().unwrap_or_default();
                if cwd == "/" || cwd.is_empty() {
                    format!("/{}", path)
                } else {
                    format!("{}/{}", cwd, path)
                }
            };
            Endpoint::Unix(UnixEndpoint::Path(abs_path))
        }
        other => other,
    };

    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();

    let files_ref = task.process.files();
    let is_nonblock = files_ref
        .lock()
        .get_file(sockfd as usize)
        .map(|f| f.is_nonblock())
        .unwrap_or(false);

    // 先尝试初始化连接（只做一次）
    match socket.connect(&endpoint) {
        Ok(n) => return n as isize,
        Err(SyscallErr::EAGAIN) => {} // 需要 wait_io
        Err(e) => {
            log::info!("[sys_connect] connect failed: {:?}", e);
            return -(e as isize);
        }
    }

    // 握手未完成，进入等待队列等待状态变化
    if let Some(wait_queue) = socket.connect_wait_queue() {
        if is_nonblock {
            // Linux 语义：非阻塞 connect 返回 EINPROGRESS（不是 EAGAIN）
            // 应用通过 poll(EPOLLOUT) 等待连接完成
            log::info!("[sys_connect] nonblock → EINPROGRESS");
            return -(SyscallErr::EINPROGRESS as isize);
        } else {
            NET_INTERFACE.poll();
            match WaitQueue::wait_until_interruptible(wait_queue, || {
                match socket.try_connect_without_poll() {
                    Ok(n) => Some(n as isize),
                    Err(SyscallErr::EAGAIN) => None,
                    Err(e) => Some(-(e as isize)),
                }
            }) {
                crate::task::WaitResult::Ready(value) => value,
                crate::task::WaitResult::Interrupted => {
                    crate::task::RestartKind::RestartSys.syscall_result()
                }
                crate::task::WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
            }
        }
    } else {
        wait_io(|| socket.try_connect(), is_nonblock)
    }
}
