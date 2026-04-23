#[allow(unused)]
use crate::{
    fs::{file_descriptor::FileDescriptor, file_trait::File, OpenFlags},
    net::{raw::RawSocket, tcp::TcpSocket, udp::UdpSocket},
    task::current_task,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::{collections::BTreeMap, sync::Arc, sync::Weak, vec::Vec};

use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpEndpoint, IpListenEndpoint};
use spin::Mutex;

pub mod adapter;
pub mod address;
pub mod config;
mod raw;
mod tcp;
mod udp;
mod unix;

pub type Fd = usize;

pub use tcp::TCP_MSS;
pub use unix::make_unix_socket_pair;
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

pub trait Socket: File {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet;
    fn listen(&self) -> SyscallRet;
    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> SyscallRet;
    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet;
    fn socket_type(&self) -> SocketType;
    fn recv_buf_size(&self) -> usize;
    fn send_buf_size(&self) -> usize;
    fn set_recv_buf_size(&self, size: usize);
    fn set_send_buf_size(&self, size: usize);
    fn loacl_endpoint(&self) -> IpListenEndpoint;
    fn remote_endpoint(&self) -> Option<IpEndpoint>;
    fn shutdown(&self, how: u32) -> GeneralRet<()>;
    fn set_nagle_enabled(&self, enabled: bool) -> SyscallRet;
    fn set_keep_alive(&self, enabled: bool) -> SyscallRet;
    fn reuse_addr(&self) -> SyscallRet;
    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet;
    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet;
}

impl dyn Socket {
    pub fn alloc(domain: u32, socket_type: u32, protocol: u32) -> GeneralRet<usize> {
        log::info!("[Socket::new] domain: {}", domain);
        let pure_type = socket_type & SOCK_TYPE_MASK;
        if domain == AF_INET6 as u32 {
            log::warn!("[Socket::alloc] AF_INET6 is not supported yet!");
            return Err(SyscallErr::EAFNOSUPPORT); // 或者写成 Err(SyscallErr::EPFNOSUPPORT)
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
                    // current_process().inner_handler(|proc| {
                    //     let fd = proc.fd_table.alloc_fd()?;
                    //     proc.fd_table.put(fd, FdInfo::new(socket.clone(), flags));
                    //     proc.socket_table.insert(fd, socket);
                    //     Ok(fd)
                    // })
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket.clone()))
                        .unwrap();
                    current_tcb.socket_table.lock().insert(fd, socket);
                    Ok(fd)
                } else if pure_type == SocketType::SOCK_STREAM.bits() {
                    let socket = TcpSocket::new();
                    let socket = Arc::new(socket);
                    // current_process().inner_handler(|proc| {
                    //     let fd = proc.fd_table.alloc_fd()?;
                    //     proc.fd_table.put(fd, FdInfo::new(socket.clone(), flags));
                    //     proc.socket_table.insert(fd, socket);
                    //     Ok(fd)
                    // })
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket.clone()))
                        .unwrap();
                    current_tcb.socket_table.lock().insert(fd, socket);
                    Ok(fd)
                } else if pure_type == SocketType::SOCK_RAW.bits() {
                    let socket = RawSocket::new(protocol);
                    let socket = Arc::new(socket);
                    let current_tcb = current_task().unwrap();
                    let fd = current_tcb
                        .files
                        .lock()
                        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket.clone()))
                        .unwrap();
                    current_tcb.socket_table.lock().insert(fd, socket);
                    Ok(fd)
                } else {
                    Err(SyscallErr::EINVAL)
                }
            }
            AF_UNIX => {
                Ok(4)
                // todo!()
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
        let local_endpoint = self.loacl_endpoint();
        let local_endpoint = address::to_endpoint(local_endpoint);
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

pub struct SocketTable(BTreeMap<Fd, Arc<dyn Socket>>);

impl SocketTable {
    pub const fn new() -> Self {
        Self(BTreeMap::new())
    }
    pub fn insert(&mut self, key: Fd, value: Arc<dyn Socket>) {
        self.0.insert(key, value);
    }
    pub fn get_ref(&self, fd: Fd) -> Option<&Arc<dyn Socket>> {
        self.0.get(&fd)
    }
    #[allow(unused)]
    pub fn take(&mut self, fd: Fd) -> Option<Arc<dyn Socket>> {
        self.0.remove(&fd)
    }
    pub fn from_another(socket_table: &SocketTable) -> GeneralRet<Self> {
        let mut ret = BTreeMap::new();
        for (sockfd, socket) in socket_table.0.iter() {
            ret.insert(*sockfd, socket.clone());
        }
        Ok(Self(ret))
    }
    pub fn can_bind(
        &self,
        endpoint: IpListenEndpoint,
        target_sock: &Arc<dyn Socket>,
    ) -> Option<(Fd, Arc<dyn Socket>)> {
        // for (sockfd, socket) in self.0.clone() {
        //     if socket.socket_type().contains(SocketType::SOCK_DGRAM) {
        //         if socket.loacl_endpoint().eq(&endpoint) {
        //             log::info!("[SockTable::can_bind] find port exist");
        //             return Some((sockfd, socket));
        //         }
        //     }
        // }
        // None
        log::info!(
            "[SockTable::can_bind] check bind for endpoint {:?} with type {:?}",
            endpoint,
            target_sock.socket_type()
        );
        let target_pure_type = target_sock.socket_type().bits() & SOCK_TYPE_MASK;
        for (sockfd, socket) in self.0.iter() {
            let pure_type = socket.socket_type().bits() & SOCK_TYPE_MASK;
            let local = socket.loacl_endpoint();
            if pure_type != target_pure_type {
                log::info!(
                    "[SockTable::can_bind] skip socket with different type: {:?}",
                    socket.socket_type()
                );
                continue;
            }
            if local.port != endpoint.port || endpoint.port == 0 {
                continue;
            }

            let addr_confilct = match (local.addr, endpoint.addr) {
                (Some(local_addr), Some(endpoint_addr)) => local_addr == endpoint_addr,
                (None, _) | (_, None) => true,
            };
            if addr_confilct {
                if pure_type == SocketType::SOCK_DGRAM.bits() {
                    let reuse_enabled_on_exist = match socket.reuse_addr() {
                        Ok(enabled) => true,
                        Err(_) => false,
                    };
                    let reuse_enabled_on_target = match target_sock.reuse_addr() {
                        Ok(enabled) => true,
                        Err(_) => false,
                    };
                    if reuse_enabled_on_exist && reuse_enabled_on_target {
                        log::info!("[SockTable::can_bind] Bypass conflict because both sockets have SO_REUSEADDR enabled");
                        continue;
                    }
                    if socket.remote_endpoint().is_some() {
                        log::info!("[SockTable::can_bind] Bypass conflict because existing UDP socket is already connected to a remote");
                        continue;
                    }
                }
                log::info!(
                    "[SockTable::can_bind] Confilct local {:?} with endpoint {:?}",
                    local,
                    endpoint
                );
                return Some((*sockfd, socket.clone()));
            }
        }
        None
    }
}
