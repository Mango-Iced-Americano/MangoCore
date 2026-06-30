use core::convert::TryFrom;
use log::info;

use crate::net::posix::PosixArgsSocketType;
use crate::net::{PSOCK, Socket};

/// 创建 socket 并返回 fd。
///
/// # Semantics
///
/// 从 raw `u32` 参数解析 `PSOCK` 类型和 `SOCK_NONBLOCK`/`SOCK_CLOEXEC` 标志位，
/// 委托给 `Socket::alloc()` 分配对应协议族的 socket 对象（`TcpSocket`/`UdpSocket`/
/// `RawSocket`/`UnixStreamSocket`/`UnixDatagramSocket`/`PacketSocket`）。
///
/// # Errors
///
/// - `-EINVAL`：`socket_type` 的纯类型字段无效（非 1..=10）。
/// - `-EAFNOSUPPORT`：`domain` 不在支持列表中（参见 `Socket::alloc` 的 match 分支）。
///
/// # Linux Compatibility
///
/// 仅支持 `AF_INET`、`AF_INET6`、`AF_UNIX`、`AF_NETLINK`、`AF_PACKET`。
/// `AF_UNSPEC` 回退到 `AF_INET` 的处理与 smoltcp 路由策略一致。
/// 对 IP socket，`domain=AF_INET6` 创建 `IpVersion::Ipv6`，其他创建 `IpVersion::Ipv4`。
pub fn sys_socket(domain: u32, socket_type: u32, protocol: u32) -> isize {
    info!(
        "[sys_socket] domain: {}, type: {}, protocol: {}",
        domain, socket_type, protocol
    );
    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
    let is_nonblock = type_arg.is_nonblock();
    let is_cloexec = type_arg.is_cloexec();
    let result = match crate::net::Socket::alloc(domain, psock, protocol, is_nonblock, is_cloexec) {
        Ok(sockfd) => {
            info!("[sys_socket] new sockfd: {}", sockfd);
            sockfd as isize
        }
        Err(e) => {
            info!("[sys_socket] new sockfd failed");
            -(e as isize)
        }
    };
    result
}
