// pub mod adapter; // 已禁用：纯 loopback 模式，无需物理网卡适配器
pub mod config;
mod macros;
pub mod posix;
pub mod socket;
pub mod syscall;

pub use spin::Mutex;

// ——— 从 socket 子模块重新导出关键类型，以保持外部引用路径 backward compatibility ———
//
// 例如 `crate::net::Socket`, `crate::net::SocketType`, `crate::net::AF_INET` 等
// 依然有效，无需修改全域的 import 路径。

// 地址解析模块：实际位于 socket::inet::common::address
pub use socket::inet::common::address;

// Socket 核心类型
pub use socket::{
    AF_INET, AF_INET6, AF_UNIX, AF_UNSPEC,
    SHUT_RD, SHUT_RDWR, SHUT_WR,
    SOCK_TYPE_MASK, SocketType,
    MAX_BUFFER_SIZE,
    UDP_SOCKETS, UDP_SOCKETS_TO_REMOVE,
    TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE,
    RAW_SOCKETS, RAW_SOCKETS_TO_REMOVE,
    GATEWAY, LOCAL_IP,
    Socket, SocketFile, Endpoint,
    TcpInfo, TCP_MSS,
    Fd,
    make_unix_socket_pair,
    wake_tcp_waiters, wake_raw_waiters,
};
// SOCK_TYPE_MASK is pub(crate), re-exported above works
