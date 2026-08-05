use crate::net::socket::inet::common::port::{AutoBindPurpose, PortManager};
use crate::task::current_task;

/// 将 socket 标记为被动模式，准备接受连接。
///
/// # Semantics
///
/// 委托 `socket.listen()`，`_backlog` 参数被忽略（内核内部使用固定 `BACKLOG_SIZE`）。
/// 当前不支持 backlog 调整，但接受任意值以保证 ABI 兼容性。
///
/// # Errors
///
/// - 由 `socket.listen()` 产生（如 `-EOPNOTSUPP`：UDP socket 不支持 listen）。
///
/// # Linux Compatibility
///
/// 简化实现：始终使用固定的 backlog 值。`SO_RCVBUF` 的 backlog 关联语义
/// （Linux 3.x+ `somaxconn`）未实现。
pub fn sys_listen(sockfd: u32, _backlog: u32) -> isize {
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();
    if let Err(error) = PortManager::ensure_auto_bound(
        &task,
        &socket,
        None,
        AutoBindPurpose::Listen,
    ) {
        return -(error as isize);
    }
    //socket.listen().unwrap() as isize
    match socket.listen() {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}
