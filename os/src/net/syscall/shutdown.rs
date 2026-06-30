use crate::utils::error::SyscallErr;

/// 关闭 socket 的单向或双向 I/O。
///
/// # Semantics
///
/// 委托 `socket.shutdown(how)`。`how` 取值：
/// `SHUT_RD=0`、`SHUT_WR=1`、`SHUT_RDWR=2`。
///
/// # Linux Compatibility
///
/// 行为与 Linux 6.6 `shutdown(2)` 一致。底层 TCP 实现由 `Inner::shutdown()`
/// 处理状态转换。
pub fn sys_sock_shutdown(sockfd: u32, how: u32) -> isize {
    log::info!("[sys_shutdown] sockfd {}, how {}", sockfd, how);
    let socket = crate::get_socket!(sockfd);
    let ret = socket.shutdown(how);
    match ret {
        Ok(_) => 0 as isize,
        Err(errno) => -(errno as isize),
    }
}
