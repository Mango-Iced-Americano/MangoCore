//! Unix 域 socket 核心模块
//!
//! 提供 `UnixEndpoint`、`UnixEndpointBound` 等核心类型，
//! 以及 `create_unix_socket()` 和 `make_unix_socket_pair()` 工厂函数。
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/mod.rs` 设计。

pub mod datagram;
pub mod ring_buffer;
pub mod stream;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::FileDescriptor;
use crate::net::{Endpoint, Socket, SocketFile, PSOCK};
use crate::task::current_task;
use crate::utils::error::{SyscallErr, SyscallRet};

// ── UnixEndpoint ────────────────────────────────────────────────────

/// Unix 域 socket 地址
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnixEndpoint {
    /// 文件系统路径（如 `/tmp/socket.sock`）
    Path(String),
    /// 抽象命名空间（以 NUL 开头，`name` 不含前导 NUL）
    Abstract(Vec<u8>),
    /// 未命名（匿名 socket）
    Unnamed,
}

// ── UnixEndpointBound ───────────────────────────────────────────────

/// 已绑定的 Unix 端点。
///
/// 当前骨架阶段直接包装 `UnixEndpoint`，后续可扩展为包含：
/// - 文件系统 inode 引用（Path 变体）
/// - 抽象命名空间表引用（Abstract 变体）
#[derive(Clone, Debug, PartialEq)]
pub enum UnixEndpointBound {
    /// 文件系统路径绑定
    Path(String),
    /// 抽象命名空间绑定
    Abstract(Vec<u8>),
    /// 未命名绑定
    Unnamed,
}

impl From<UnixEndpointBound> for UnixEndpoint {
    fn from(bound: UnixEndpointBound) -> Self {
        match bound {
            UnixEndpointBound::Path(p) => UnixEndpoint::Path(p),
            UnixEndpointBound::Abstract(v) => UnixEndpoint::Abstract(v),
            UnixEndpointBound::Unnamed => UnixEndpoint::Unnamed,
        }
    }
}

impl From<UnixEndpoint> for UnixEndpointBound {
    fn from(ep: UnixEndpoint) -> Self {
        match ep {
            UnixEndpoint::Path(p) => UnixEndpointBound::Path(p),
            UnixEndpoint::Abstract(v) => UnixEndpointBound::Abstract(v),
            UnixEndpoint::Unnamed => UnixEndpointBound::Unnamed,
        }
    }
}

impl From<UnixEndpoint> for Endpoint {
    fn from(ep: UnixEndpoint) -> Self {
        Endpoint::Unix(ep)
    }
}

impl From<UnixEndpointBound> for Endpoint {
    fn from(bound: UnixEndpointBound) -> Self {
        Endpoint::Unix(UnixEndpoint::from(bound))
    }
}

// ── 工厂函数 ────────────────────────────────────────────────────────

/// 根据 socket 类型创建对应的 Unix 域 socket。
pub fn create_unix_socket(
    socket_type: PSOCK,
    is_nonblock: bool,
) -> Result<Arc<dyn Socket>, SyscallErr> {
    match socket_type {
        PSOCK::Stream => {
            let socket: Arc<dyn Socket> = Arc::new(stream::UnixStreamSocket::new(is_nonblock));
            Ok(socket)
        }
        PSOCK::Datagram | PSOCK::Raw => {
            let socket: Arc<dyn Socket> = datagram::UnixDatagramSocket::new(is_nonblock);
            Ok(socket)
        }
        PSOCK::SeqPacket => {
            // SOCK_SEQPACKET 映射到 stream（带消息边界）
            let socket: Arc<dyn Socket> = Arc::new(stream::UnixStreamSocket::new(is_nonblock));
            Ok(socket)
        }
        _ => Err(SyscallErr::ESOCKTNOSUPPORT),
    }
}

/// 创建一对已连接的 Unix stream socket（用于 `socketpair` 系统调用）。
///
/// 返回 `(socket_a, socket_b)`，两者通过环形缓冲区双向连接。
pub fn make_unix_socket_pair(
    is_nonblock: bool,
) -> (Arc<stream::UnixStreamSocket>, Arc<stream::UnixStreamSocket>) {
    let (inner_a, inner_b) =
        stream::inner::Connected::new_pair(stream::inner::UNIX_STREAM_DEFAULT_BUF_SIZE);
    let socket_a = Arc::new(stream::UnixStreamSocket::new_connected(
        inner_a,
        is_nonblock,
    ));
    let socket_b = Arc::new(stream::UnixStreamSocket::new_connected(
        inner_b,
        is_nonblock,
    ));
    (socket_a, socket_b)
}

/// 将 socket 包装为 SocketFile 并分配 fd。
pub fn alloc_socket_fd(
    socket: Arc<dyn Socket>,
    is_nonblock: bool,
    is_cloexec: bool,
) -> Result<usize, SyscallErr> {
    let socket_file = Arc::new(SocketFile::new(socket));
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    let fd = task
        .files
        .lock()
        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file))
        .map_err(|_| SyscallErr::ENFILE)?;
    Ok(fd)
}
