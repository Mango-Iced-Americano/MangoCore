use crate::net::config::lookup_source_ip;
use crate::net::{config::NET_INTERFACE, Mutex, Socket, MAX_BUFFER_SIZE};
use crate::{
    net::address,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use log::info;
use smoltcp::{
    iface::SocketHandle,
    phy::PacketMeta,
    socket::{
        self,
        udp::{PacketMetadata, SendError, UdpMetadata},
        AnySocket,
    },
    wire::{IpAddress, IpEndpoint, IpListenEndpoint},
};

use crate::net::config::NetInterfaceInner;
use crate::net::{UDP_SOCKETS, UDP_SOCKETS_TO_REMOVE};
use crate::task::WaitQueue;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: SocketHandle,
    recv_waiters: Mutex<WaitQueue>,
    send_waiters: Mutex<WaitQueue>,
}

struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,
    local_endpoint: Option<IpListenEndpoint>,
    rx_queue: VecDeque<(alloc::vec::Vec<u8>, IpEndpoint)>,
    last_recv_addr: Option<IpEndpoint>,
    recvbuf_size: usize,
    sendbuf_size: usize,
    reuse_addr: bool,
}

impl Socket for UdpSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        log::info!("[Udp::bind] bind to {:?}", addr);
        self.inner.lock().local_endpoint = Some(addr);
        NET_INTERFACE.poll();
        NET_INTERFACE
            .udp_socket(self.socket_handler, |socket| {
                socket.bind(addr).ok().ok_or(SyscallErr::EINVAL)
            })
            .ok_or(SyscallErr::EAGAIN)??;
        NET_INTERFACE.poll();
        Ok(0)
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> crate::utils::error::SyscallRet {
        let remote_endpoint = address::endpoint(addr_buf)?;
        log::info!("[Udp::connect] connect to {:?}", remote_endpoint);
        {
            let mut inner = self.inner.lock();
            inner.remote_endpoint = Some(remote_endpoint);
        }
        NET_INTERFACE.poll();
        let local_ep = NET_INTERFACE
            .udp_socket(self.socket_handler, |socket| {
                let local = socket.endpoint();
                info!("[Udp::connect] local: {:?}", local);
                if local.port == 0 {
                    info!("[Udp::connect] don't have local");
                    let src_ip = lookup_source_ip(remote_endpoint.addr);
                    let port =
                        crate::net::socket::inet::common::PortManager::alloc_ephemeral_port();

                    let endpoint = IpListenEndpoint {
                        addr: Some(src_ip),
                        port,
                    };

                    let ret = socket.bind(endpoint);
                    if ret.is_err() {
                        match ret.err().unwrap() {
                            socket::udp::BindError::Unaddressable => {
                                info!("[Udp::bind] unaddr");
                                return Err(SyscallErr::EINVAL);
                            }
                            socket::udp::BindError::InvalidState => {
                                info!("[Udp::bind] invaild state");
                                return Err(SyscallErr::EINVAL);
                            }
                        }
                    }
                    log::info!("[Udp::bind] bind to {:?}", endpoint);
                    Ok(endpoint)
                } else {
                    Ok(local)
                }
            })
            .ok_or(SyscallErr::EAGAIN)??;
        self.inner.lock().local_endpoint = Some(local_ep);
        NET_INTERFACE.poll();
        Ok(0)
    }

    fn accept(
        &self,
        _sockfd: u32,
        _addr: usize,
        _addrlen: usize,
    ) -> crate::utils::error::SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn socket_type(&self) -> crate::net::SocketType {
        crate::net::SocketType::SOCK_DGRAM
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size;
    }

    fn local_endpoint(&self) -> IpListenEndpoint {
        NET_INTERFACE.poll();
        let local = NET_INTERFACE.udp_socket(self.socket_handler, |socket| socket.endpoint());
        NET_INTERFACE.poll();
        local.unwrap_or(IpListenEndpoint {
            addr: None,
            port: 0,
        })
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        self.inner.lock().remote_endpoint
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        log::info!("[UdpSocket::shutdown] how {}", how);
        Ok(())
    }

    fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn set_keep_alive(&self, _enabled: bool) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn reuse_addr(&self) -> SyscallRet {
        let reuse_addr = self.inner.lock().reuse_addr;
        Ok(reuse_addr as usize)
    }

    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet {
        self.inner.lock().reuse_addr = enabled;
        Ok(0)
    }

    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet {
        todo!();
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        self.try_recvmsg(buf).map(|(size, _)| size)
    }

    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        // EMSGSIZE: UDP 最大负载 65535 - 20(IP头) - 8(UDP头) = 65507
        if buf.len() > 65507 {
            return Err(SyscallErr::EMSGSIZE);
        }
        let remote = self
            .inner
            .lock()
            .remote_endpoint
            .ok_or(SyscallErr::ENOTCONN)?;
        let meta = UdpMetadata {
            endpoint: remote,
            meta: PacketMeta::default(),
        };
        // 不调用 poll，只做一次尝试
        NET_INTERFACE
            .udp_socket(self.socket_handler, |socket| {
                if !socket.can_send() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.send_slice(buf, meta) {
                    Ok(()) => Ok(buf.len() as isize),
                    Err(SendError::Unaddressable) => Err(SyscallErr::ENOTCONN),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<IpEndpoint>), SyscallErr> {
        // 从 rx_queue 非阻塞取一包数据 + 源地址
        let mut inner = self.inner.lock();
        if let Some((data, remote)) = inner.rx_queue.pop_front() {
            let copy_len = data.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            // 如果未 connect，将 remote 更新到 inner（与现有 try_recv 行为一致）
            if inner.remote_endpoint.is_none() {
                inner.remote_endpoint = Some(remote);
            }
            inner.last_recv_addr = Some(remote);
            Ok((copy_len as isize, Some(remote)))
        } else {
            Err(SyscallErr::EAGAIN)
        }
    }

    fn try_sendmsg(&self, buf: &[u8], dest: Option<IpEndpoint>) -> Result<isize, SyscallErr> {
        // EMSGSIZE check
        if buf.len() > 65507 {
            return Err(SyscallErr::EMSGSIZE);
        }
        // 确定目标地址：优先使用 dest 参数，否则用已连接的 remote_endpoint
        let remote = match dest {
            Some(ep) => ep,
            None => self
                .inner
                .lock()
                .remote_endpoint
                .ok_or(SyscallErr::ENOTCONN)?,
        };
        let meta = UdpMetadata {
            endpoint: remote,
            meta: PacketMeta::default(),
        };
        NET_INTERFACE
            .udp_socket(self.socket_handler, |socket| {
                if !socket.can_send() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.send_slice(buf, meta) {
                    Ok(()) => Ok(buf.len() as isize),
                    Err(SendError::Unaddressable) => Err(SyscallErr::ENOTCONN),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }
    fn last_recv_addr(&self) -> Option<IpEndpoint> {
        self.inner.lock().last_recv_addr.take()
    }

    fn socket_r_ready(&self) -> bool {
        NET_INTERFACE.poll();
        !self.inner.lock().rx_queue.is_empty()
    }

    fn socket_w_ready(&self) -> bool {
        NET_INTERFACE
            .udp_socket(self.socket_handler, |socket| socket.can_send())
            .unwrap_or(false)
    }

    fn socket_hang_up(&self) -> bool {
        false
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.send_waiters)
    }

    fn recv_ready(&self) -> bool {
        !self.inner.lock().rx_queue.is_empty()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }
}

impl UdpSocket {
    pub fn new() -> Self {
        let tx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1024],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1024],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = socket::udp::Socket::new(rx_buf, tx_buf);
        let socket_handler = NET_INTERFACE.add_socket(socket).unwrap();
        log::info!("[UdpSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        Self {
            inner: Mutex::new(UdpSocketInner {
                remote_endpoint: None,
                local_endpoint: None,
                rx_queue: VecDeque::new(),
                last_recv_addr: None,
                recvbuf_size: MAX_BUFFER_SIZE,
                sendbuf_size: MAX_BUFFER_SIZE,
                reuse_addr: false,
            }),
            socket_handler,
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
        }
    }
    pub fn register_udp_socket(socket: &Arc<Self>) {
        UDP_SOCKETS.lock().push(Arc::downgrade(socket));
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        // log::info!(
        //     "[UdpSocket::drop] drop socket {}, remoteep {:?}, localep {:?}",
        //     self.socket_handler,
        //     self.inner.lock().remote_endpoint,
        //     self.inner.lock().local_endpoint
        // );
        // NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
        //     if socket.is_open() {
        //         socket.close();
        //     }
        // });
        // NET_INTERFACE.remove(self.socket_handler);
        UDP_SOCKETS_TO_REMOVE.lock().push(self.socket_handler);
    }
}

impl UdpSocket {
    fn _read<'a>(&'a self, buf: &'a mut [u8]) -> GeneralRet<usize> {
        NET_INTERFACE.poll();
        // let ret = NET_INTERFACE.udp_socket(self.socket_handler, |socket| {
        //     if !socket.can_recv() {
        //         // panic!();
        //         log::trace!("[UdpRecvFuture::poll] cannot recv yet");
        //         return Err(SyscallErr::EAGAIN);
        //     }
        //     log::info!("[UdpRecvFuture::poll] start to recv...");
        //     let (ret, meta) = socket.recv_slice(buf).ok().ok_or(SyscallErr::ENOTCONN)?;
        //     let remote = Some(meta.endpoint);
        //     info!(
        //         "[UdpRecvFuture::poll] {:?} <- {:?}",
        //         socket.endpoint(),
        //         remote
        //     );
        //     self.inner.lock().remote_endpoint = remote;
        //     log::debug!("[UdpRecvFuture::poll] recv {} bytes", ret);
        //     Ok(ret)
        // });
        // NET_INTERFACE.poll();
        // match ret {
        //     Ok(result) => return GeneralRet::Ok(result),
        //     Err(err) => return GeneralRet::Err(err),
        // }
        let mut inner = self.inner.lock();
        if let Some((data, remote)) = inner.rx_queue.pop_front() {
            let copy_len = data.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            // 对于未connect的socket，更新最近通信的对端(recvfrom需要)
            if inner.remote_endpoint.is_none() {
                // 或者在 syscall recvfrom 里处理对端信息
                inner.remote_endpoint = Some(remote);
            }
            log::debug!("[UdpSocket] read {} bytes from {:?}", copy_len, remote);
            return GeneralRet::Ok(copy_len);
        }
        GeneralRet::Err(SyscallErr::EAGAIN)
    }
}

// 新的分发函数：直接接收 NetInterfaceInner，避免重复获取锁导致死锁！
pub fn dispatch_udp_packets(inner: &mut NetInterfaceInner) {
    let mut os_socks = UDP_SOCKETS.lock();

    // 顺便清理一下已经被 drop 掉的 socket
    os_socks.retain(|w| w.strong_count() > 0);

    for (handle, socket) in inner.sockets.iter_mut() {
        // 尝试把这个 socket 识别为 UDP 类型
        if let Some(udp_sock) = smoltcp::socket::udp::Socket::downcast_mut(socket) {
            // 只要这个底层缓冲区里有包，就全部抽干
            while udp_sock.can_recv() {
                let mut buf = vec![0u8; 2048];
                if let Ok((size, meta)) = udp_sock.recv_slice(&mut buf) {
                    buf.truncate(size);

                    // 3. 拿到包了，调用我们写的打分函数，找到它在 OS 层对应的 UdpSocket
                    // 注意：这里的 local 信息直接从当前遍历到的 udp_sock 拿
                    if let Some(target_os_sock) =
                        find_best_match(&os_socks, udp_sock.endpoint(), meta.endpoint)
                    {
                        target_os_sock
                            .inner
                            .lock()
                            .rx_queue
                            .push_back((buf, meta.endpoint));
                        target_os_sock.recv_waiters.lock().wake_at_most(1);
                    } else {
                        // 如果没人认领这个包（比如 iperf3 已经关了），就丢弃
                        log::warn!(
                            "[UDP Dispatch] No OS Socket matched packet from {:?}, dropping",
                            meta.endpoint
                        );
                    }
                }
            }
        }
    }
}

// 寻找最匹配的 OS UdpSocket
fn find_best_match(
    sockets: &[Weak<UdpSocket>],
    local: IpListenEndpoint,
    remote: IpEndpoint,
) -> Option<Arc<UdpSocket>> {
    let mut best_match = None;
    let mut best_score = 0;

    for weak_sock in sockets {
        if let Some(sock) = weak_sock.upgrade() {
            let inner = sock.inner.lock();
            let local_match = inner.local_endpoint.map(|l| l.port).unwrap_or(0) == local.port;

            // 如果本地端口匹配，计算匹配得分
            if local_match {
                let score = match inner.remote_endpoint {
                    // 1. 完美匹配：这是专门负责这个远端的 Socket
                    Some(ep) if ep == remote => 2,

                    // 2. 名花有主：已经 connect 了别的地址，绝不能收这个包
                    Some(_) => 0,

                    // 3. 备胎/监听者：没有 connect 任何地址，可以接纳新来的包
                    None => 1,
                };
                if score > best_score {
                    best_score = score;
                    best_match = Some(sock.clone());
                }
            }
        }
    }
    best_match
}
