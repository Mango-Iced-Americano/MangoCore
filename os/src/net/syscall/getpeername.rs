/// 获取 peer 的远程地址。
///
/// # Semantics
///
/// 调用 `Socket::peer_addr()` 获取 `remote_endpoint` 并写入用户空间 `sockaddr`。
/// 遵循 `prevalidate_sockaddr` → `prevalidate_socklen_value` → `probe_user_write`
/// → `remote_endpoint` 的顺序，确保 `-EFAULT`/`-EINVAL` 优先于 `-ENOTCONN`
/// （Linux `getpeername01` 测试期望此优先级）。
///
/// # Errors
///
/// - `-EFAULT`：`addr` 为 NULL、`addrlen` 未对齐，或用户缓冲区不可写。
/// - `-EINVAL`：`socklen_t` 负值或 `<2`。
/// - `-ENOTCONN`：socket 无 `remote_endpoint`。
///
/// # Linux Compatibility
///
/// 参数验证优先级完全匹配 Linux 6.6：NULL `addr` 或未对齐 `addrlen` → `-EFAULT`，
/// 在 `-ENOTCONN` 之前返回（`getpeername01` 期望此顺序）。
pub fn sys_getpeername(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    match socket.peer_addr(addr, addrlen) {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}
