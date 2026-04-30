use crate::{
    fs::{
        directory_tree::DirectoryTreeNode, fat32::DiskInodeType, file_descriptor::FileDescriptor,
        file_trait::File, Dirent, OpenFlags, PageCache, SeekWhence, Stat,
    },
    mm::UserBuffer,
    net::socket::inet::datagram::udp::UdpSocket,
    net::socket::inet::raw::raw::RawSocket,
    net::socket::inet::stream::TcpStreamSocket,
    task::current_task,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::collections::VecDeque;
use alloc::{
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};

use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
use spin::Mutex;

use crate::task::manager::WaitQueue;

// pub mod adapter; // 已禁用：纯 loopback 模式，无需物理网卡适配器
pub mod address;
pub mod config;
mod macros;
pub mod posix;
pub mod socket;

pub type Fd = usize;

pub use crate::net::socket::inet::stream::TcpInfo;
pub use crate::net::socket::inet::stream::TCP_MSS;
pub use crate::net::socket::unix::unix::make_unix_socket_pair;
// pub use unix::UNIX_SOCKET_BUF_MANAGER;

/// domain
pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;

/// shutdown
#[allow(unused)]
pub const SHUT_RD: u32 = 0;
pub const SHUT_WR: u32 = 1;
#[allow(unused)]
pub const SHUT_RDWR: u32 = 2;

const SOCK_TYPE_MASK: u32 = 0xF;
bitflags! {
    /// socket type
    pub struct SocketType: u32 {
        /// for TCP
        const SOCK_STREAM = 1 ;
        /// for UDP
        const SOCK_DGRAM = 2;
        //
        const SOCK_RAW = 3;

        const SOCK_RDM = 4;

        const SOCK_SEQPACKET = 5;

        const SOCK_DCCP = 6;

        const SOCK_PACKET = 10;

        const SOCK_CLOEXEC = 1 << 19;

        const SOCK_NONBLOCK = 0x800;
    }
}

// pub const MAX_BUFFER_SIZE: usize = 1 << 15;
// pub const MAX_BUFFER_SIZE: usize = 1 << 16;
pub const MAX_BUFFER_SIZE: usize = 64 * 1024;

// 定义全局的 UDP Sockets 集合
pub static UDP_SOCKETS: Mutex<Vec<Weak<UdpSocket>>> = Mutex::new(Vec::new());
pub static UDP_SOCKETS_TO_REMOVE: Mutex<Vec<SocketHandle>> = Mutex::new(Vec::new());

// tcp
pub static TCP_SOCKETS: Mutex<Vec<Weak<TcpStreamSocket>>> = Mutex::new(Vec::new());
pub static TCP_SOCKETS_TO_REMOVE: Mutex<Vec<SocketHandle>> = Mutex::new(Vec::new());

// raw
pub static RAW_SOCKETS: Mutex<Vec<(SocketHandle, Weak<RawSocket>)>> = Mutex::new(Vec::new());
pub static RAW_SOCKETS_TO_REMOVE: Mutex<Vec<SocketHandle>> = Mutex::new(Vec::new());

pub static GATEWAY: IpAddress = IpAddress::v4(10, 0, 2, 2);
pub static LOCAL_IP: IpAddress = IpAddress::v4(10, 0, 2, 15);

pub trait Socket: Send + Sync {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet;
    fn listen(&self) -> SyscallRet;
    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> SyscallRet;
    /// 尝试建立连接一次（不阻塞），检查一次握手状态。
    /// 返回 Ok(0) 表示已建立，Err(EAGAIN) 表示尚在握手/需重试。
    fn try_connect(&self) -> Result<isize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet;
    fn socket_type(&self) -> SocketType;
    fn recv_buf_size(&self) -> usize;
    fn send_buf_size(&self) -> usize;
    fn set_recv_buf_size(&self, size: usize);
    fn set_send_buf_size(&self, size: usize);
    fn local_endpoint(&self) -> IpListenEndpoint;
    fn remote_endpoint(&self) -> Option<IpEndpoint>;
    fn shutdown(&self, how: u32) -> GeneralRet<()>;
    fn set_nagle_enabled(&self, enabled: bool) -> SyscallRet;
    fn set_keep_alive(&self, enabled: bool) -> SyscallRet;
    fn reuse_addr(&self) -> SyscallRet;
    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet;
    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet;

    /// 尝试接收消息（recvmsg 用）。
    /// 成功时返回 (字节数, 可选的源地址)。
    /// 源地址仅 UDP/RAW 有意义，TCP/Unix 返回 None。
    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<IpEndpoint>), SyscallErr> {
        // 默认实现：委托 try_recv，不返回地址
        let n = self.try_recv(buf)?;
        Ok((n, None))
    }

    /// 尝试发送消息（sendmsg 用）。
    /// dest 为 None 时使用 socket 已连接的远程端点。
    fn try_sendmsg(&self, buf: &[u8], dest: Option<IpEndpoint>) -> Result<isize, SyscallErr> {
        // UDP RawSocket 子类会重写此方法
        let _ = dest;
        self.try_send(buf)
    }

    /// 获取最近一次接收到的源地址（仅 UDP 有意义）。
    fn last_recv_addr(&self) -> Option<IpEndpoint> {
        None
    }

    /// 尝试接收数据，不阻塞。
    /// 不会调用 poll、不会睡眠、不会调度。成功时返回收到的字节数 (isize)。
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr>;

    /// 尝试发送数据，不阻塞。
    /// 不会调用 poll、不会睡眠、不会调度。成功时返回发送的字节数 (isize)。
    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr>;

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

    /// 获取发送等待队列引用
    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    /// 获取连接等待队列引用
    fn connect_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    /// 获取 accept 等待队列引用
    fn accept_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        None
    }

    /// 获取 TCP 状态 (Linux TCP_* 枚举值)，非 TCP socket 返回 None
    fn tcp_state(&self) -> Option<u8> {
        None
    }
}

/// 统一的 Socket 文件包装类。
/// 所有 TcpStreamSocket/UdpSocket/RawSocket 都通过此结构体对外体现为 File。
pub struct SocketFile {
    pub inner: Arc<dyn Socket>,
}

impl SocketFile {
    pub fn new(socket: Arc<dyn Socket>) -> Self {
        Self { inner: socket }
    }
}

impl File for SocketFile {
    fn deep_clone(&self) -> Arc<dyn File> {
        Arc::new(SocketFile {
            inner: self.inner.clone(),
        })
    }

    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }

    fn read(&self, _offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
        match self.inner.try_recv(buf) {
            Ok(n) => n as usize,
            Err(e) => e.as_errno_ret(),
        }
    }

    fn write(&self, _offset: Option<&mut usize>, buf: &[u8]) -> usize {
        match self.inner.try_send(buf) {
            Ok(n) => n as usize,
            Err(e) => e.as_errno_ret(),
        }
    }

    fn r_ready(&self) -> bool {
        self.inner.socket_r_ready()
    }

    fn w_ready(&self) -> bool {
        self.inner.socket_w_ready()
    }

    fn read_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        let mut data = vec![0u8; buf.len];
        match self.inner.try_recv(&mut data) {
            Ok(s) => {
                let mut offset = 0usize;
                let mut remain = s as usize;
                for b in buf.buffers.into_iter() {
                    let copy_len = remain.min(b.len());
                    b[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
                    offset += copy_len;
                    remain -= copy_len;
                    if remain == 0 {
                        break;
                    }
                }
                s as usize
            }
            Err(e) => e.as_errno_ret(),
        }
    }

    fn write_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        let mut data = vec![0u8; buf.len];
        let mut offset = 0;
        for b in buf.buffers.into_iter() {
            data[offset..offset + b.len()].copy_from_slice(&b);
            offset += b.len();
        }
        self.write(None, &data)
    }

    fn get_size(&self) -> usize {
        0
    }

    fn get_stat(&self) -> Stat {
        unsafe { core::mem::zeroed() }
    }

    fn get_file_type(&self) -> DiskInodeType {
        DiskInodeType::Socket
    }

    fn is_dir(&self) -> bool {
        false
    }

    fn is_file(&self) -> bool {
        false
    }

    fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {}

    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
        None
    }

    fn open(&self, _flags: OpenFlags, _special_use: bool) -> Arc<dyn File> {
        panic!("socket open should not be called");
    }

    fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
        Err(-(crate::syscall::errno::EISDIR as isize))
    }

    fn create(&self, _name: &str, _file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        Err(-(crate::syscall::errno::EISDIR as isize))
    }

    fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize> {
        Err(-(crate::syscall::errno::EISDIR as isize))
    }

    fn unlink(&self, _delete: bool) -> Result<(), isize> {
        Err(-(crate::syscall::errno::EISDIR as isize))
    }

    fn get_dirent(&self, _count: usize) -> Vec<Dirent> {
        Vec::new()
    }

    fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
        Err(-(crate::syscall::errno::ESPIPE as isize))
    }

    fn modify_size(&self, _diff: isize) -> Result<(), isize> {
        Err(-(crate::syscall::errno::EPERM as isize))
    }

    fn truncate_size(&self, _new_size: usize) -> Result<(), isize> {
        Err(-(crate::syscall::errno::EPERM as isize))
    }

    fn set_timestamp(&self, _ctime: Option<usize>, _atime: Option<usize>, _mtime: Option<usize>) {}

    fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
        Err(())
    }

    fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
        Err(())
    }

    fn oom(&self) -> usize {
        0
    }

    fn hang_up(&self) -> bool {
        self.inner.socket_hang_up()
    }

    fn ioctl(&self, _cmd: u32, _argp: usize) -> isize {
        crate::syscall::errno::ENOTTY
    }

    fn fcntl(&self, _cmd: u32, _arg: u32) -> isize {
        0
    }
}

impl dyn Socket {
    pub fn alloc(domain: u32, socket_type: u32, protocol: u32) -> GeneralRet<usize> {
        log::info!("[Socket::new] domain: {}", domain);
        let pure_type = socket_type & SOCK_TYPE_MASK;
        if domain == AF_INET6 as u32 {
            log::warn!("[Socket::alloc] AF_INET6 is not supported yet!");
            return Err(SyscallErr::EAFNOSUPPORT);
        }

        match domain as u16 {
            AF_INET => {
                let socket_type = SocketType::from_bits(socket_type).ok_or(SyscallErr::EINVAL)?;
                // let flags = if socket_type.contains(SocketType::SOCK_CLOEXEC) {
                //     OpenFlags::O_RDWR | OpenFlags::O_CLOEXEC
                // } else {
                //     OpenFlags::O_RDWR
                // };
                let is_nonblock = socket_type.contains(SocketType::SOCK_NONBLOCK);
                let is_cloexec = socket_type.contains(SocketType::SOCK_CLOEXEC);
                // info!("[Socket::alloc] flags: {:?}", flags);
                if pure_type == SocketType::SOCK_DGRAM.bits() {
                    let socket = UdpSocket::new();
                    let socket = Arc::new(socket);
                    UdpSocket::register_udp_socket(&socket);
                    let socket_file = Arc::new(SocketFile::new(socket));
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file))
                        .unwrap();
                    Ok(fd)
                } else if pure_type == SocketType::SOCK_STREAM.bits() {
                    let socket = TcpStreamSocket::new();
                    let socket = Arc::new(socket);
                    TcpStreamSocket::register_tcp_socket(&socket);
                    let socket_file = Arc::new(SocketFile::new(socket));
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file))
                        .unwrap();
                    Ok(fd)
                } else if pure_type == SocketType::SOCK_RAW.bits() {
                    let socket = RawSocket::new(protocol);
                    let socket = Arc::new(socket);
                    RawSocket::register_raw_socket(&socket);
                    let socket_file = Arc::new(SocketFile::new(socket));
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file))
                        .unwrap();
                    Ok(fd)
                } else {
                    Err(SyscallErr::EINVAL)
                }
            }
            AF_UNIX => {
                // Ok(4)
                todo!()
                // let socket = UnixSocket::new();
                // let socket = Arc::new(Socket::UnixSocket(socket));
                // current_process().inner_handler(|proc| {
                //     let fd = proc.fd_table.alloc_fd()?;
                //     proc.fd_table.put(fd, socket.clone());
                //     proc.socket_table.insert(fd, socket);
                //     Ok(fd)
                // })
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }
    pub fn addr(self: &Arc<Self>, addr: usize, addrlen: usize) -> SyscallRet {
        let local_endpoint = self.local_endpoint();
        // let local_endpoint = address::to_endpoint(local_endpoint);
        let local_endpoint = address::listen_to_ip_endpoint_preserve(local_endpoint);
        address::fill_with_endpoint(local_endpoint, addr, addrlen)
    }
    pub fn peer_addr(self: &Arc<Self>, addr: usize, addrlen: usize) -> SyscallRet {
        let remote_endpoint = self.remote_endpoint();
        if remote_endpoint.is_none() {
            return Err(SyscallErr::ENOTCONN);
        }
        address::fill_with_endpoint(remote_endpoint.unwrap(), addr, addrlen)
    }
}

/// 在每次 poll 后，遍历所有 TCP_SOCKETS，唤醒其等待队列
pub fn wake_tcp_waiters() {
    let mut remove_indices = Vec::new();
    let sockets = TCP_SOCKETS.lock();
    for (i, weak_socket) in sockets.iter().enumerate() {
        if let Some(socket) = weak_socket.upgrade() {
            // socket.wake_if_ready();
            socket.wake_wait_queues();
        } else {
            remove_indices.push(i);
        }
    }
    // 不能在持有 sockets 锁时修改，所以先收集再处理
    drop(sockets);
    if !remove_indices.is_empty() {
        let mut sockets = TCP_SOCKETS.lock();
        for &i in remove_indices.iter().rev() {
            if i < sockets.len() {
                sockets.remove(i);
            }
        }
    }
}

/// 在每次 poll 后，遍历所有 RAW_SOCKETS，唤醒其 recv_waiters
pub fn wake_raw_waiters() {
    let mut remove_indices = Vec::new();
    let sockets = RAW_SOCKETS.lock();
    for (i, (handler, weak_socket)) in sockets.iter().enumerate() {
        if let Some(socket) = weak_socket.upgrade() {
            let can_recv = crate::net::config::NET_INTERFACE
                .raw_socket(*handler, |s| s.can_recv())
                .unwrap_or(false);
            if can_recv {
                socket.recv_waiters.lock().wake_at_most(1);
            }
        } else {
            remove_indices.push(i);
        }
    }
    // 不能在持有 sockets 锁时修改，所以先收集再处理
    drop(sockets);
    if !remove_indices.is_empty() {
        let mut sockets = RAW_SOCKETS.lock();
        for &i in remove_indices.iter().rev() {
            if i < sockets.len() {
                sockets.remove(i);
            }
        }
    }
}
