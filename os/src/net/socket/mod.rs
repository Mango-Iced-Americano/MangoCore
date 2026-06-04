pub mod common;
pub mod inet;
pub mod unix;
pub mod netlink;

use crate::{
    fs::{
        fat32::DiskInodeType,
        vfs, vfs::FileFlags, Dirent, OpenFlags, SeekWhence, Stat,
    },
    mm::{UserBuffer, UserBufferWriter, UserPtr, UserPtrMut},
    net::{
        posix::PosixArgsSocketType,
        socket::inet::{
            datagram::udp::UdpSocket,
            raw::raw::RawSocket,
            stream::{inner::with_tcp_mut, TcpSocket},
        },
        syscall::common::MsgFlags,
    },
    task::{current_task, WaitQueue},
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::collections::VecDeque;
use alloc::{
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::any::Any;
use core::convert::TryFrom;

use crate::net::routing::RouteSocketHandle;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv6Address};
use spin::Mutex;

use crate::fs::vfs::{
    FilePrivateData, FileType, IndexNode, InodeFlags, InodeMode, Metadata,
};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::file_system::FileSystem as NewFileSystem;
use crate::fs::vfs::file_system::{FileSystem, FsInfo, SuperBlock};
use crate::timer::TimeSpec;

/// Socket 虚拟文件系统（用于 IndexNode::fs()）
#[derive(Debug)]
struct SocketFS;

impl FileSystem for SocketFS {
    fn root_inode(&self) -> Arc<dyn IndexNode> {
        panic!("SocketFS has no root inode")
    }
    fn info(&self) -> FsInfo {
        FsInfo { blk_dev_id: 0, max_name_len: 0, features: vec!["socketfs"] }
    }
    fn name(&self) -> &str { "socketfs" }
    fn super_block(&self) -> SuperBlock { SuperBlock::default() }
    fn as_any_ref(&self) -> &dyn Any { self }
}

lazy_static::lazy_static! {
    static ref SOCKET_FS: Arc<SocketFS> = Arc::new(SocketFS);
}

use crate::net::socket::inet::common::address;

pub type Fd = usize;

pub use crate::net::socket::inet::stream::TcpInfo;
pub use crate::net::socket::inet::stream::TCP_MSS;
pub use crate::net::socket::unix::datagram::UnixDatagramSocket;
pub use crate::net::socket::unix::make_unix_socket_pair;
pub use crate::net::socket::unix::stream::UnixStreamSocket;
pub use crate::net::socket::unix::UnixEndpoint;
/// domain
pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;
pub const AF_NETLINK: u16 = 16;

/// shutdown
#[allow(unused)]
pub const SHUT_RD: u32 = 0;
pub const SHUT_WR: u32 = 1;
#[allow(unused)]
pub const SHUT_RDWR: u32 = 2;

/// POSIX socket 纯类型枚举（仅包含类型，不包含 NONBLOCK / CLOEXEC 等控制标志）。
/// 对标 DragonOS `kernel/src/net/socket/posix/types.rs` 的 `PSOCK`。
/// 用于全内核的 socket 类型匹配，取代旧的 `SocketType` bitflags。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PSOCK {
    /// SOCK_STREAM（对应 TCP）
    Stream = 1,
    /// SOCK_DGRAM（对应 UDP）
    Datagram = 2,
    /// SOCK_RAW
    Raw = 3,
    /// SOCK_RDM
    RDM = 4,
    /// SOCK_SEQPACKET
    SeqPacket = 5,
    /// SOCK_DCCP
    DCCP = 6,
    /// SOCK_PACKET
    Packet = 10,
}

impl TryFrom<PosixArgsSocketType> for PSOCK {
    type Error = SyscallErr;
    fn try_from(x: PosixArgsSocketType) -> Result<Self, Self::Error> {
        match x.types().bits() {
            1 => Ok(PSOCK::Stream),
            2 => Ok(PSOCK::Datagram),
            3 => Ok(PSOCK::Raw),
            4 => Ok(PSOCK::RDM),
            5 => Ok(PSOCK::SeqPacket),
            6 => Ok(PSOCK::DCCP),
            10 => Ok(PSOCK::Packet),
            _ => Err(SyscallErr::EINVAL),
        }
    }
}

/// POSIX SOCK_TYPE 掩码，仅用于 `PosixArgsSocketType` 类型解析。
/// 新代码应直接使用 `PSOCK` 枚举，无需手动 mask。
pub(crate) const SOCK_TYPE_MASK: u32 = 0xF;

// pub const MAX_BUFFER_SIZE: usize = 1 << 15;
// pub const MAX_BUFFER_SIZE: usize = 1 << 16;
pub const MAX_BUFFER_SIZE: usize = 64 * 1024;

// 定义全局的 UDP Sockets 集合
pub static UDP_SOCKETS: Mutex<Vec<Weak<UdpSocket>>> = Mutex::new(Vec::new());
pub static UDP_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>> = Mutex::new(Vec::new());

// tcp
pub static TCP_SOCKETS: Mutex<Vec<Weak<TcpSocket>>> = Mutex::new(Vec::new());
pub static TCP_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>> = Mutex::new(Vec::new());

// raw
pub static RAW_SOCKETS: Mutex<Vec<(RouteSocketHandle, Weak<RawSocket>)>> = Mutex::new(Vec::new());
pub static RAW_SOCKETS_TO_REMOVE: Mutex<Vec<RouteSocketHandle>> = Mutex::new(Vec::new());


// ── Endpoint 枚举 ─────────────────────────────────────────────────────

/// 统一的 socket 端点抽象，覆盖所有地址族。
/// 对标 DragonOS `kernel/src/net/socket/endpoint.rs` 的 `Endpoint` 枚举，
/// 当前仅实现了 IP 和 Unix 两种变体。
#[derive(Debug, Clone, PartialEq)]
pub enum Endpoint {
    /// AF_INET / AF_INET6 端点
    Ip(IpEndpoint),
    /// AF_UNIX 端点
    Unix(UnixEndpoint),
    /// 未指定（AF_UNSPEC）
    Unspecified,
}

impl Endpoint {
    /// 从原始 sockaddr 字节解析为 Endpoint（根据 sa_family 自动分发）。
    pub fn from_sockaddr(addr_buf: &[u8]) -> Result<Self, SyscallErr> {
        if addr_buf.len() < 2 {
            return Err(SyscallErr::EINVAL);
        }
        let family = u16::from_ne_bytes([addr_buf[0], addr_buf[1]]);
        match family {
            AF_INET => {
                if addr_buf.len() < 8 {
                    return Err(SyscallErr::EINVAL);
                }
                let port = u16::from_be_bytes([addr_buf[2], addr_buf[3]]);
                let ip =
                    Ipv4Address::from_bytes(&[addr_buf[4], addr_buf[5], addr_buf[6], addr_buf[7]]);
                Ok(Endpoint::Ip(IpEndpoint::new(IpAddress::Ipv4(ip), port)))
            }
            AF_INET6 => {
                if addr_buf.len() < 24 {
                    return Err(SyscallErr::EINVAL);
                }
                let port = u16::from_be_bytes([addr_buf[2], addr_buf[3]]);
                let mut ip_bytes = [0u8; 16];
                ip_bytes.copy_from_slice(&addr_buf[8..24]);
                let ip = Ipv6Address(ip_bytes);
                Ok(Endpoint::Ip(IpEndpoint::new(IpAddress::Ipv6(ip), port)))
            }
            AF_UNIX => {
                let path_bytes = &addr_buf[2..];
                if path_bytes.is_empty() || path_bytes[0] == 0 {
                    if path_bytes.len() > 1 {
                        Ok(Endpoint::Unix(UnixEndpoint::Abstract(
                            path_bytes[1..].to_vec(),
                        )))
                    } else {
                        Ok(Endpoint::Unix(UnixEndpoint::Unnamed))
                    }
                } else {
                    // 文件系统路径（以 \0 截断）
                    let len = path_bytes
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(path_bytes.len());
                    let path_str =
                        core::str::from_utf8(&path_bytes[..len]).map_err(|_| SyscallErr::EINVAL)?;
                    Ok(Endpoint::Unix(UnixEndpoint::Path(path_str.to_string())))
                }
            }
            AF_UNSPEC => Ok(Endpoint::Unspecified),
            _ => Err(SyscallErr::EAFNOSUPPORT),
        }
    }

    /// 获取端口的便捷方法（非 IP 端点返回 0）。
    pub fn port(&self) -> u16 {
        match self {
            Endpoint::Ip(ep) => ep.port,
            _ => 0,
        }
    }

    /// 将 Endpoint 写入用户空间 sockaddr 缓冲区，并更新 addrlen。
    pub fn fill_sockaddr(&self, addr: usize, addrlen: usize) -> SyscallRet {
        match self {
            Endpoint::Ip(ep) => address::fill_with_endpoint(*ep, addr, addrlen),
            Endpoint::Unix(unix_ep) => unix::fill_with_endpoint(unix_ep, addr, addrlen),
            Endpoint::Unspecified => {
                // NULL 指针检查
                if addr == 0 || addrlen == 0 {
                    return Err(SyscallErr::EFAULT);
                }
                let task = current_task().unwrap();
                let token = task.get_user_token();

                // 解引用 addrlen，检查缓冲区大小
                let addrlen_ptr = UserPtrMut::<u32>::from_addr(addrlen);
                let capacity = match addrlen_ptr.read(token) {
                    Ok(len) => len as usize,
                    Err(_) => return Err(SyscallErr::EFAULT),
                };
                if capacity < 2 {
                    return Err(SyscallErr::EINVAL);
                }

                // 写入 AF_UNSPEC（2 字节）
                let write_len = 2;
                let mut user_buf =
                    UserBufferWriter::new(token, addr as *mut u8, write_len)
                        .map_err(|_| SyscallErr::EFAULT)?;
                user_buf
                    .write_from(&AF_UNSPEC.to_ne_bytes())
                    .map_err(|_| SyscallErr::EFAULT)?;
                // 回写 addrlen
                addrlen_ptr
                    .write(token, &2u32)
                    .map_err(|_| SyscallErr::EFAULT)?;
                Ok(0)
            }
        }
    }
}

pub trait Socket: Send + Sync {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet;
    fn listen(&self) -> SyscallRet;
    fn connect(&self, endpoint: &Endpoint) -> SyscallRet;
    /// 尝试建立连接一次（不阻塞），检查一次握手状态。
    /// 返回 Ok(0) 表示已建立，Err(EAGAIN) 表示尚在握手/需重试。
    fn try_connect(&self) -> Result<isize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet;
    fn socket_type(&self) -> PSOCK;
    fn recv_buf_size(&self) -> usize;
    fn send_buf_size(&self) -> usize;
    fn set_recv_buf_size(&self, size: usize);
    fn set_send_buf_size(&self, size: usize);
    fn local_endpoint(&self) -> Option<Endpoint>;
    fn remote_endpoint(&self) -> Option<Endpoint>;
    fn shutdown(&self, how: u32) -> GeneralRet<()>;
    fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn set_keep_alive(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn reuse_addr(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn peer_creds(&self) -> Result<(u32, u32, u32), SyscallErr> {
        Err(SyscallErr::ENOPROTOOPT)
    }
    fn set_reuse_addr(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn join_multicast_group(&self) -> SyscallRet {
        Ok(0)
    }
    fn leave_multicast_group(&self) -> SyscallRet {
        Err(SyscallErr::EADDRNOTAVAIL)
    }
    fn send_to(&self, _buf: &[u8], _dest: Endpoint) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 尝试接收消息（recvmsg 用）。
    /// 成功时返回 (字节数, 可选的源地址)。
    /// 源地址仅 UDP/RAW 有意义，TCP/Unix 返回 None。
    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        // 默认实现：委托 try_recv，不返回地址
        let n = self.try_recv(buf)?;
        Ok((n, None))
    }

    /// 尝试发送消息（sendmsg 用）。
    /// dest 为 None 时使用 socket 已连接的远程端点。
    fn try_sendmsg(
        &self,
        buf: &[u8],
        dest: Option<Endpoint>,
        _flags: MsgFlags,
    ) -> Result<isize, SyscallErr> {
        // UDP RawSocket 子类会重写此方法
        let _ = dest;
        self.try_send(buf, _flags)
    }

    /// 获取最近一次接收到的源地址（仅 UDP 有意义）。
    fn last_recv_addr(&self) -> Option<Endpoint> {
        None
    }

    /// 尝试接收数据，不阻塞。
    /// 不会调用 poll、不会睡眠、不会调度。成功时返回收到的字节数 (isize)。
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr>;

    /// 尝试发送数据，不阻塞。
    /// 不会调用 poll、不会睡眠、不会调度。成功时返回发送的字节数 (isize)。
    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr>;

    fn push_netlink_message(&self, _data: Vec<u8>) -> Result<(), SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn is_netlink_socket(&self) -> bool {
        false
    }

    /// poll/select 相关：是否可读（不阻塞）
    fn socket_r_ready(&self) -> bool {
        true
    }

    /// poll/select 相关：是否可写（不阻塞）
    fn socket_w_ready(&self) -> bool {
        true
    }

    /// poll/select 相关：是否挂起
    fn socket_hang_up(&self) -> bool {
        false
    }

    /// 获取接收等待队列引用（用于事件驱动阻塞）
    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    fn recv_event_queue(&self) -> Option<&EventWaitQueue> {
        None
    }

    /// 获取发送等待队列引用
    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    fn send_event_queue(&self) -> Option<&EventWaitQueue> {
        None
    }

    /// 获取连接等待队列引用
    fn connect_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    fn connect_event_queue(&self) -> Option<&EventWaitQueue> {
        None
    }

    /// 获取 accept 等待队列引用
    fn accept_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    fn accept_event_queue(&self) -> Option<&EventWaitQueue> {
        None
    }

    /// 获取 TCP 状态 (Linux TCP_* 枚举值)，非 TCP socket 返回 None
    fn tcp_state(&self) -> Option<u8> {
        None
    }

    fn recv_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }

    fn accept_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn connect_ready(&self) -> bool {
        self.socket_w_ready()
    }

    /// Unix stream 专用：把新建立的 Connected 推入 listener 的 incoming 队列。
    /// 默认返回 EOPNOTSUPP（非 Unix stream socket 不支持此操作）。
    fn push_pending_connected(
        &self,
        _conn: crate::net::socket::unix::stream::inner::Connected,
    ) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }
}

/// 统一的 Socket 文件包装类。
/// 所有 TcpStreamSocket/UdpSocket/RawSocket 都通过此结构体对外体现为 File。
pub struct SocketFile {
    pub inner: Arc<dyn Socket>,
}

impl core::fmt::Debug for SocketFile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SocketFile").finish()
    }
}

impl SocketFile {
    pub fn new(socket: Arc<dyn Socket>) -> Self {
        Self { inner: socket }
    }
}

impl IndexNode for SocketFile {
    fn read_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &mut [u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner.try_recv(buf).map(|n| n as usize)
    }

    fn write_at(
        &self,
        _offset: usize,
        _len: usize,
        buf: &[u8],
        _data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        self.inner
            .try_send(buf, MsgFlags::empty())
            .map(|n| n as usize)
    }

    fn metadata(&self) -> Result<Metadata, SyscallErr> {
        Ok(Metadata {
            dev_id: 0,
            inode_id: 0,
            size: 0,
            blk_size: 0,
            blocks: 0,
            atime: TimeSpec::new(),
            mtime: TimeSpec::new(),
            ctime: TimeSpec::new(),
            file_type: FileType::Socket,
            mode: InodeMode::S_IFSOCK | InodeMode::from_bits_truncate(0o777),
            nlinks: 1,
            uid: 0,
            gid: 0,
            flags: InodeFlags::empty(),
            raw_dev: 0,
        })
    }

    fn is_stream(&self) -> bool {
        true
    }

    fn poll(&self, _private_data: &FilePrivateData) -> Result<usize, SyscallErr> {
        let mut revents: usize = 0;
        if self.inner.socket_r_ready() {
            revents |= EPollEvent::EPOLLIN.bits();
        }
        if self.inner.socket_w_ready() {
            revents |= EPollEvent::EPOLLOUT.bits();
        }
        if self.inner.socket_hang_up() {
            revents |= EPollEvent::EPOLLHUP.bits();
        }
        Ok(revents)
    }

    fn read_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        self.inner
            .recv_wait_queue()
            .or_else(|| self.inner.accept_wait_queue())
    }

    fn read_event_queue(&self) -> Option<&EventWaitQueue> {
        self.inner
            .recv_event_queue()
            .or_else(|| self.inner.accept_event_queue())
    }

    fn write_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        self.inner
            .send_wait_queue()
            .or_else(|| self.inner.connect_wait_queue())
    }

    fn write_event_queue(&self) -> Option<&EventWaitQueue> {
        self.inner
            .send_event_queue()
            .or_else(|| self.inner.connect_event_queue())
    }

    fn ioctl(
        &self,
        cmd: u32,
        argp: usize,
        _private_data: spin::MutexGuard<FilePrivateData>,
    ) -> Result<usize, SyscallErr> {
        if cmd >= 0x8900 && cmd <= 0x89FF {
            return crate::net::ioctl::siocgif_dispatch(cmd, argp);
        }
        Err(SyscallErr::ENOTTY)
    }

    fn fs(&self) -> Arc<dyn NewFileSystem> {
        SOCKET_FS.clone()
    }

    fn as_any_ref(&self) -> &dyn Any {
        self
    }
}

impl dyn Socket {
    pub fn alloc(
        domain: u32,
        psock: PSOCK,
        protocol: u32,
        is_nonblock: bool,
        is_cloexec: bool,
    ) -> GeneralRet<usize> {
        log::info!("[Socket::new] domain: {}, psock: {:?}", domain, psock);
        if domain == AF_INET6 as u32 {
            log::warn!("[Socket::alloc] AF_INET6 is not supported yet!");
            return Err(SyscallErr::EAFNOSUPPORT);
        }
        let alloc_socket_fd = |socket_file: Arc<dyn crate::fs::vfs::IndexNode>| -> GeneralRet<usize> {
            let mut flags = FileFlags::O_RDWR;
            if is_nonblock { flags.insert(FileFlags::O_NONBLOCK); }
            let vf = vfs::File::new_without_open(socket_file, flags, vfs::FileType::Socket);
            let files_ref = current_task().unwrap().process.files();
            let result = files_ref.lock().alloc_fd(vf, is_cloexec);
            result
        };
        match domain as u16 {
            AF_INET | AF_UNSPEC => {
                log::info!("[Socket::new] domain: {} -> treating as AF_INET", domain);
                match psock {
                    PSOCK::Datagram => {
                        let socket = UdpSocket::new();
                        let socket = Arc::new(socket);
                        UdpSocket::register_udp_socket(&socket);
                        let socket_file = Arc::new(SocketFile::new(socket));
                        alloc_socket_fd(socket_file)
                    }
                    PSOCK::Stream => {
                        let socket = TcpSocket::new();
                        let socket = Arc::new(socket);
                        TcpSocket::register_tcp_socket(&socket);
                        let socket_file = Arc::new(SocketFile::new(socket));
                        alloc_socket_fd(socket_file)
                    }
                    PSOCK::Raw => {
                        let socket = RawSocket::new(protocol);
                        let socket = Arc::new(socket);
                        RawSocket::register_raw_socket(&socket);
                        let socket_file = Arc::new(SocketFile::new(socket));
                        alloc_socket_fd(socket_file)
                    }
                    _ => Err(SyscallErr::EINVAL),
                }
            }
            AF_UNIX => {
                log::info!("[Socket::new] domain: AF_UNIX");
                match psock {
                    PSOCK::Stream => {
                        let socket: Arc<dyn Socket> = Arc::new(UnixStreamSocket::new(is_nonblock));
                        let socket_file = Arc::new(SocketFile::new(socket));
                        alloc_socket_fd(socket_file)
                    }
                    PSOCK::Datagram | PSOCK::Raw => {
                        let socket = UnixDatagramSocket::new(is_nonblock);
                        let socket: Arc<dyn Socket> = socket;
                        let socket_file = Arc::new(SocketFile::new(socket));
                        alloc_socket_fd(socket_file)
                    }
                    _ => return Err(SyscallErr::EAFNOSUPPORT),
                }
            }
            AF_NETLINK => match psock {
                PSOCK::Raw | PSOCK::Datagram => {
                    let socket: Arc<dyn Socket> = Arc::new(crate::net::socket::netlink::NetlinkSocket::new(protocol));
                    let socket_file = Arc::new(SocketFile::new(socket));
                    alloc_socket_fd(socket_file)
                }
                _ => Err(SyscallErr::EINVAL),
            },
            _ => Err(SyscallErr::EAFNOSUPPORT),
        }
    }

    /// 检查 addr/addrlen 用户指针的有效性，返回 (addr_ptr, addrlen_ptr)。
    /// 符合 Linux 语义：在检查连接状态前先验证参数。
    fn prevalidate_sockaddr(addr: usize, addrlen: usize) -> Result<(), SyscallErr> {
        // NULL 指针 → EFAULT
        if addr == 0 || addrlen == 0 {
            return Err(SyscallErr::EFAULT);
        }
        // 未对齐的 addrlen 指针 → EFAULT（RISC-V 未对齐访问可能静默成功）
        if addrlen % 4 != 0 {
            return Err(SyscallErr::EFAULT);
        }
        Ok(())
    }

    pub fn addr(self: &Arc<Self>, addr: usize, addrlen: usize) -> SyscallRet {
        // Linux: 先验证参数有效性，再检查连接状态
        Self::prevalidate_sockaddr(addr, addrlen)?;
        // 在检查连接状态前，先读取并验证 *addrlen，确保无效的 socklen 值
        // 不会被 ENOTCONN 掩盖（getpeername01 期望 EINVAL 优先于 ENOTCONN）
        Self::prevalidate_socklen_value(addrlen)?;
        let endpoint = self.local_endpoint().ok_or(SyscallErr::ENOTCONN)?;
        endpoint.fill_sockaddr(addr, addrlen)
    }
    pub fn peer_addr(self: &Arc<Self>, addr: usize, addrlen: usize) -> SyscallRet {
        // Linux: 先验证参数有效性，再检查连接状态
        Self::prevalidate_sockaddr(addr, addrlen)?;
        // 在检查连接状态前，先读取并验证 *addrlen
        Self::prevalidate_socklen_value(addrlen)?;
        // 在连接状态检查前，先探针写入 addr 缓冲区 — 无效指针→EFAULT
        // （getpeername01 期望 EFAULT 优先于 ENOTCONN）
        Self::probe_user_write(addr)?;
        let endpoint = self.remote_endpoint().ok_or(SyscallErr::ENOTCONN)?;
        endpoint.fill_sockaddr(addr, addrlen)
    }

    fn probe_user_write(addr: usize) -> Result<(), SyscallErr> {
        if addr == 0 {
            return Err(SyscallErr::EFAULT);
        }
        let task = current_task().ok_or(SyscallErr::EINVAL)?;
        let token = task.get_user_token();
        // 探针读取 — 无效地址会触发 page fault → EFAULT
        UserPtr::<u8>::from_addr(addr).read(token).map(|_| ()).map_err(|_| SyscallErr::EFAULT)
    }

    /// 读取并验证用户空间的 socklen_t 值，优先于连接状态检查。
    /// Linux 上 socklen_t 是 signed int，负值为无效 → EINVAL。
    fn prevalidate_socklen_value(addrlen: usize) -> Result<(), SyscallErr> {
        let task = current_task().ok_or(SyscallErr::EINVAL)?;
        let token = task.get_user_token();
        let val = match UserPtr::<u32>::from_addr(addrlen).read(token) {
            Ok(val) => val,
            Err(_) => return Err(SyscallErr::EFAULT),
        };
        // socklen_t 在 Linux 上是 signed int，负值 → EINVAL
        if (val as i32) < 0 {
            return Err(SyscallErr::EINVAL);
        }
        // 太小（至少需要 sa_family 的 2 字节）→ EINVAL
        if val < 2 {
            return Err(SyscallErr::EINVAL);
        }
        Ok(())
    }
}

/// 在每次 poll 后，遍历所有 TCP_SOCKETS，唤醒其等待队列
pub fn wake_tcp_waiters() {
    let mut live_sockets: Vec<Arc<TcpSocket>> = Vec::new();
    let mut remove_indices = Vec::new();
    {
        let sockets = TCP_SOCKETS.lock();
        for (i, weak_socket) in sockets.iter().enumerate() {
            if let Some(socket) = weak_socket.upgrade() {
                live_sockets.push(socket);
            } else {
                remove_indices.push(i);
            }
        }
    }
    for socket in &live_sockets {
        socket.wake_if_ready();
    }
    drop(live_sockets);
    if !remove_indices.is_empty() {
        let mut sockets = TCP_SOCKETS.lock();
        for &i in remove_indices.iter().rev() {
            if i < sockets.len() {
                sockets.remove(i);
            }
        }
    }
}

/// 在每次 poll 后，遍历所有 RAW_SOCKETS，唤醒其等待队列
pub fn wake_raw_waiters() {
    let mut live_sockets: Vec<(RouteSocketHandle, Arc<dyn Socket>)> = Vec::new();
    let mut remove_indices = Vec::new();
    {
        let sockets = RAW_SOCKETS.lock();
        for (i, (handler, weak_socket)) in sockets.iter().enumerate() {
            if let Some(socket) = weak_socket.upgrade() {
                live_sockets.push((*handler, socket));
            } else {
                remove_indices.push(i);
            }
        }
    }
    for (handler, socket) in &live_sockets {
        let can_recv = crate::net::config::NET_INTERFACE
            .raw_routed_socket(*handler, |s| s.can_recv())
            .unwrap_or(false);
        if can_recv {
            if let Some(wq) = socket.recv_event_queue() {
                wq.notify_events_at_most_if_unlocked(
                    EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM,
                    1,
                );
            }
        }
    }
    drop(live_sockets);
    if !remove_indices.is_empty() {
        let mut sockets = RAW_SOCKETS.lock();
        for &i in remove_indices.iter().rev() {
            if i < sockets.len() {
                sockets.remove(i);
            }
        }
    }
}
