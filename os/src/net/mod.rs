pub mod adapter;
pub mod config;
pub mod iface;
pub mod ioctl;
mod macros;
pub mod net_core;
pub mod posix;
pub mod router_device;
pub mod routing;
pub mod socket;
pub mod syscall;


pub use spin::Mutex;

// ——— 从 socket 子模块重新导出关键类型，以保持外部引用路径 backward compatibility ———
//
// 例如 `crate::net::Socket`, `crate::net::PSOCK`, `crate::net::AF_INET` 等
// 依然有效，无需修改全域的 import 路径。

// 地址解析模块：实际位于 socket::inet::common::address
pub use socket::inet::common::address;

// Socket 核心类型
pub use socket::{
    make_unix_socket_pair, wake_raw_waiters, wake_tcp_waiters, Endpoint, Fd, Socket, SocketFile,
    TcpInfo, AF_INET, AF_INET6, AF_NETLINK, AF_PACKET, AF_UNIX, AF_UNSPEC, MAX_BUFFER_SIZE, PSOCK,
    RAW_SOCKETS, RAW_SOCKETS_TO_REMOVE, SHUT_RD, SHUT_RDWR, SHUT_WR, TCP_MSS, TCP_SOCKETS,
    TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS, UDP_SOCKETS_TO_REMOVE,
};
// PSOCK replaces the old SocketType bitflags (which mixed type + SOCK_NONBLOCK/SOCK_CLOEXEC)
