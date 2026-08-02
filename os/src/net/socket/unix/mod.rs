//! Unix 域 socket 核心模块
//!
//! 提供 `UnixEndpoint`、`UnixEndpointBound` 等核心类型，
//! 以及 `create_unix_socket()` 和 `make_unix_socket_pair()` 工厂函数。
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/mod.rs` 设计。

pub mod datagram;
pub mod ns;
pub mod ring_buffer;
pub mod stream;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::vfs::event::EventWaitQueue;
use crate::fs::vfs::{self, FileFlags};
use crate::mm::{UserBufferWriter, UserPtrMut};
use crate::net::{Endpoint, Socket, SocketFile, PSOCK};
use crate::task::current_task;
use crate::utils::error::{SyscallErr, SyscallRet};

lazy_static::lazy_static! {
    /// 存放命名地址 Unix Socket 的全局映射。
    /// Key: 绝对路径（如 `/tmp/my.sock`）
    /// Value: 指向 Socket 的弱引用
    pub static ref PATH_TABLE: Mutex<BTreeMap<String, Weak<dyn Socket>>> =
        Mutex::new(BTreeMap::new());
}

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
            // The Unix stream implementation has the byte-pipe semantics Rust
            // uses for its process error channel; MSG_EOR/message boundaries are
            // not implemented yet.
            let socket: Arc<dyn Socket> = Arc::new(stream::UnixStreamSocket::new(is_nonblock));
            Ok(socket)
        }
        PSOCK::RDM | PSOCK::DCCP | PSOCK::Packet => Err(SyscallErr::ESOCKTNOSUPPORT),
    }
}

/// 创建一对已连接的 Unix socket（用于 `socketpair` 系统调用）。
///
/// 返回 `(socket_a, socket_b)`，两者通过环形缓冲区双向连接。
pub fn make_unix_socket_pair(
    is_nonblock: bool,
    socket_type: PSOCK,
) -> Result<(Arc<dyn Socket>, Arc<dyn Socket>), SyscallErr> {
    match socket_type {
        // Match create_unix_socket(): SeqPacket currently reuses the connected
        // stream byte-pipe until Unix message-boundary support is available.
        PSOCK::Stream | PSOCK::SeqPacket => {
            let (inner_a, inner_b) = stream::inner::Connected::new_pair(
                stream::inner::UNIX_STREAM_DEFAULT_BUF_SIZE,
                Arc::new(EventWaitQueue::new()),
                Arc::new(EventWaitQueue::new()),
                Arc::new(EventWaitQueue::new()),
                Arc::new(EventWaitQueue::new()),
            );
            let socket_a = Arc::new(stream::UnixStreamSocket::new_connected(
                inner_a,
                is_nonblock,
            ));
            let socket_b = Arc::new(stream::UnixStreamSocket::new_connected(
                inner_b,
                is_nonblock,
            ));
            Ok((socket_a, socket_b))
        }
        PSOCK::Datagram => {
            let (socket_a, socket_b) = datagram::UnixDatagramSocket::new_pair(is_nonblock);
            Ok((socket_a as Arc<dyn Socket>, socket_b as Arc<dyn Socket>))
        }
        PSOCK::Raw | PSOCK::RDM | PSOCK::DCCP | PSOCK::Packet => Err(SyscallErr::ESOCKTNOSUPPORT),
    }
}

// ── fill_with_endpoint ──────────────────────────────────────────────

/// 将 UnixEndpoint 写入用户空间 sockaddr_un 缓冲区，并回写 addrlen。
///
/// 对标 `address::_fill_with_endpoint` 的签名与职责分离模式。
/// `addr` 和 `addrlen` 是用户空间原始指针值（`usize`）。
pub fn fill_with_endpoint(ep: &UnixEndpoint, addr: usize, addrlen: usize) -> SyscallRet {
    // NULL 指针检查
    if addr == 0 || addrlen == 0 {
        return Err(SyscallErr::EFAULT);
    }
    // 对齐检查：addrlen 指针必须 4 字节对齐（RISC-V 未对齐访问可能静默成功）
    if addrlen % 4 != 0 {
        return Err(SyscallErr::EFAULT);
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();

    // 解引用 addrlen，拿到用户缓冲区的实际容量
    let addrlen_ptr = UserPtrMut::<u32>::from_addr(addrlen);
    let capacity = match addrlen_ptr.read(token) {
        Ok(len) => len as usize,
        Err(_) => return Err(SyscallErr::EFAULT),
    };

    // addrlen 太小（至少需要 2 字节容纳 sa_family）→ EINVAL
    if capacity < 2 {
        return Err(SyscallErr::EINVAL);
    }
    // socklen_t 在 Linux 上是 signed int，负值表示无效 → EINVAL
    if (capacity as i32) < 0 {
        return Err(SyscallErr::EINVAL);
    }

    // 构建 sockaddr_un 字节 (sa_family + sun_path)
    let mut data = Vec::new();
    data.extend_from_slice(&super::AF_UNIX.to_ne_bytes()); // sa_family = AF_UNIX

    let actual_len = match ep {
        UnixEndpoint::Path(path) => {
            // 文件系统路径：拷贝尽量多的字节，末尾补 NUL
            let path_bytes = path.as_bytes();
            let n = capacity.saturating_sub(2).min(path_bytes.len());
            data.extend_from_slice(&path_bytes[..n]);
            if data.len() < capacity {
                data.push(0); // NUL terminator
            }
            data.len()
        }
        UnixEndpoint::Abstract(name) => {
            // sun_path[0] = NUL, 后面跟抽象名称字节
            data.push(0);
            let n = capacity.saturating_sub(3).min(name.len());
            data.extend_from_slice(&name[..n]);
            data.len()
        }
        UnixEndpoint::Unnamed => {
            // 只有 sa_family（2 字节），sun_path 全零
            data.len() // = 2
        }
    };

    // 写入用户空间缓冲区
    let write_len = actual_len.min(capacity);
    let mut user_buf =
        UserBufferWriter::new(token, addr as *mut u8, write_len).map_err(|_| SyscallErr::EFAULT)?;
    user_buf
        .write_from(&data[..write_len])
        .map_err(|_| SyscallErr::EFAULT)?;

    // 回写实际需要的地址长度
    addrlen_ptr
        .write(token, &(actual_len as u32))
        .map_err(|_| SyscallErr::EFAULT)?;
    Ok(0)
}

/// 将 socket 包装为 SocketFile 并分配 fd。
pub fn alloc_socket_fd(
    socket: Arc<dyn Socket>,
    is_nonblock: bool,
    is_cloexec: bool,
) -> Result<usize, SyscallErr> {
    let socket_file: Arc<dyn crate::fs::vfs::IndexNode> = Arc::new(SocketFile::new(socket));
    let mut flags = FileFlags::O_RDWR;
    if is_nonblock {
        flags.insert(FileFlags::O_NONBLOCK);
    }
    let vf = vfs::File::new_without_open(socket_file, flags, vfs::FileType::Socket);
    let task = current_task().ok_or(SyscallErr::ESRCH)?;
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    fd_table.alloc_fd(vf, is_cloexec)
}
