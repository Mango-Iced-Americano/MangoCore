/// 获取 socket 的本地地址。
///
/// # Semantics
///
/// 调用 `Socket::addr()` 获取 `local_endpoint` 并写入用户空间 `sockaddr`。
/// 遵循 Linux 语义：先验证参数（NULL 指针 → `-EFAULT`，`socklen_t` 负值 → `-EINVAL`），
/// 再检查连接状态（`-ENOTCONN`）。
///
/// # Errors
///
/// - `-EFAULT`：`addr`/`addrlen` 为 NULL 或用户指针非法。
/// - `-EINVAL`：`socklen_t` 值为负或小于 2。
/// - `-ENOTCONN`：socket 未绑定或未连接（无 `local_endpoint`）。
pub fn sys_getsockname(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    match socket.addr(addr, addrlen) {
        Ok(new_fd) => new_fd as isize,
        Err(err) => -(err as isize),
    }
}
