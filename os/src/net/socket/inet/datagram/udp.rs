use crate::net::config::lookup_source_ip;
use crate::net::config::route_check;
use crate::net::routing::{InetProtocol, RouteSocketHandle};
use crate::net::syscall::common::MsgFlags;
use crate::net::{config::NET_INTERFACE, Endpoint, Mutex, Socket, MAX_BUFFER_SIZE};
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
    iface::SocketSet,
    phy::PacketMeta,
    socket::{
        self,
        udp::{PacketMetadata, SendError, UdpMetadata},
        AnySocket,
    },
    wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address},
};

use crate::net::config::NetInterfaceInner;
use crate::net::socket::inet::common::{BoundInner, PortManager};
use crate::net::{UDP_SOCKETS, UDP_SOCKETS_TO_REMOVE};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::task::WaitQueue;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: RouteSocketHandle,
    bound: Mutex<BoundInner>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}

struct UdpSocketInner {
    remote_endpoint: Option<IpEndpoint>,
    local_endpoint: Option<IpListenEndpoint>,
    rx_queue: VecDeque<(alloc::vec::Vec<u8>, IpEndpoint)>,
    last_recv_addr: Option<IpEndpoint>,
    msg_more_buf: Vec<u8>,
    recvbuf_size: usize,
    sendbuf_size: usize,
    reuse_addr: bool,
    multicast_group_joined: bool,
}

impl Socket for UdpSocket {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let addr = if ep.addr.is_unspecified() {
            IpListenEndpoint {
                addr: None,
                port: ep.port,
            }
        } else {
            IpListenEndpoint {
                addr: Some(ep.addr),
                port: ep.port,
            }
        };
        log::info!("[Udp::bind] bind to {:?}", addr);
        // 处理 port=0：分配临时端口，与 TCP Inner::bind 语义一致
        let bind_addr = if addr.port == 0 {
            let port = PortManager::alloc_ephemeral_port();
            IpListenEndpoint { port, ..addr }
        } else {
            addr
        };
        self.inner.lock().local_endpoint = Some(bind_addr);
        NET_INTERFACE.poll();
        NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| {
                socket.bind(bind_addr).ok().ok_or(SyscallErr::EINVAL)
            })
            .ok_or(SyscallErr::EAGAIN)??;
        NET_INTERFACE.poll();
        let ifindex = crate::net::net_core::ifindex_for_local_addr(bind_addr.addr);
        self.bound
            .lock()
            .bind(self.socket_handler, ifindex, bind_addr.addr, bind_addr.port);
        log::debug!("udp_bind: addr={:?} port={} ifindex={}", bind_addr.addr, bind_addr.port, ifindex);
        Ok(0)
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        // Linux: connect() to INADDR_ANY is treated as localhost
        let remote_endpoint = if ep.addr.is_unspecified() {
            IpEndpoint::new(
                crate::net::net_core::loopback_iface()
                    .and_then(|d| d.iface.ip_addrs().first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(127, 0, 0, 1)),
                ep.port,
            )
        } else {
            *ep
        };
        log::info!("[Udp::connect] connect to {:?}", remote_endpoint);
        {
            let mut inner = self.inner.lock();
            inner.remote_endpoint = Some(remote_endpoint);
        }
        NET_INTERFACE.poll();
        let local_ep = NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| {
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
        let ifindex = crate::net::net_core::ifindex_for_local_addr(local_ep.addr);
        self.bound
            .lock()
            .bind(self.socket_handler, ifindex, local_ep.addr, local_ep.port);
        log::debug!("udp_connect: remote={:?} ifindex={}", remote_endpoint, ifindex);
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

    fn socket_type(&self) -> crate::net::PSOCK {
        crate::net::PSOCK::Datagram
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

    fn local_endpoint(&self) -> Option<Endpoint> {
        NET_INTERFACE.poll();
        let local: Option<IpListenEndpoint> =
            NET_INTERFACE.udp_routed_socket(self.socket_handler, |socket| socket.endpoint());
        NET_INTERFACE.poll();
        local.map(|ep| {
            let addr = ep.addr.unwrap_or(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED));
            Endpoint::Ip(IpEndpoint::new(addr, ep.port))
        })
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        self.inner.lock().remote_endpoint.map(Endpoint::Ip)
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

    fn join_multicast_group(&self) -> SyscallRet {
        self.inner.lock().multicast_group_joined = true;
        Ok(0)
    }

    fn leave_multicast_group(&self) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.multicast_group_joined {
            inner.multicast_group_joined = false;
            Ok(0)
        } else {
            Err(SyscallErr::EADDRNOTAVAIL)
        }
    }

    fn send_to(&self, buf: &[u8], dest: Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = dest else {
            return Err(SyscallErr::EINVAL);
        };
        let _ = ep;
        todo!();
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        self.try_recvmsg(buf).map(|(size, _)| size)
    }

    fn try_send(&self, buf: &[u8], flags: MsgFlags) -> Result<isize, SyscallErr> {
        // EMSGSIZE: UDP 最大负载 65535 - 20(IP头) - 8(UDP头) = 65507
        if buf.len() > 65507 {
            return Err(SyscallErr::EMSGSIZE);
        }
        // MSG_MORE: 缓冲数据；非 MSG_MORE: 合并缓冲后发送
        let (remote, send_buf) = {
            let mut inner = self.inner.lock();
            let remote = inner.remote_endpoint.ok_or(SyscallErr::ENOTCONN)?;
            if flags.contains(MsgFlags::MSG_MORE) {
                inner.msg_more_buf.extend_from_slice(buf);
                return Ok(buf.len() as isize);
            }
            if !inner.msg_more_buf.is_empty() {
                inner.msg_more_buf.extend_from_slice(buf);
                (remote, core::mem::take(&mut inner.msg_more_buf))
            } else {
                (remote, buf.to_vec())
            }
        };
        let meta = UdpMetadata {
            endpoint: remote,
            meta: PacketMeta::default(),
        };
        if let Some(n) = self.try_deliver_local(remote, &send_buf)? {
            return Ok(n);
        }
        // 不调用 poll，只做一次尝试
        NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| {
                if !socket.can_send() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.send_slice(&send_buf, meta) {
                    Ok(()) => Ok(buf.len() as isize),
                    Err(SendError::Unaddressable) => Err(SyscallErr::ENOTCONN),
                    Err(SendError::BufferFull) => Err(SyscallErr::EAGAIN),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        // 从 rx_queue 非阻塞取一包数据 + 源地址
        let mut inner = self.inner.lock();
        if let Some((data, remote)) = inner.rx_queue.pop_front() {
            log::info!(
                "[try_recvmsg] popped {} bytes from {:?}, remaining in rx_queue={}",
                data.len(),
                remote,
                inner.rx_queue.len()
            );
            let copy_len = data.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            inner.last_recv_addr = Some(remote);
            Ok((copy_len as isize, Some(Endpoint::Ip(remote))))
        } else {
            Err(SyscallErr::EAGAIN)
        }
    }

    fn try_sendmsg(
        &self,
        buf: &[u8],
        dest: Option<Endpoint>,
        flags: MsgFlags,
    ) -> Result<isize, SyscallErr> {
        // EMSGSIZE check
        if buf.len() > 65507 {
            return Err(SyscallErr::EMSGSIZE);
        }
        // 确定目标地址：优先使用 dest 参数，否则用已连接的 remote_endpoint
        // MSG_MORE: 缓冲数据；非 MSG_MORE: 合并缓冲后发送
        let (remote, send_buf) = {
            let res = match dest {
                Some(Endpoint::Ip(ep)) => Ok(ep),
                Some(_) => Err(SyscallErr::EINVAL),
                None => self
                    .inner
                    .lock()
                    .remote_endpoint
                    .ok_or(SyscallErr::ENOTCONN),
            };
            let remote = res?;
            let mut inner = self.inner.lock();
            if flags.contains(MsgFlags::MSG_MORE) {
                inner.msg_more_buf.extend_from_slice(buf);
                return Ok(buf.len() as isize);
            }
            if !inner.msg_more_buf.is_empty() {
                inner.msg_more_buf.extend_from_slice(buf);
                (remote, core::mem::take(&mut inner.msg_more_buf))
            } else {
                (remote, buf.to_vec())
            }
        };
        let meta = UdpMetadata {
            endpoint: remote,
            meta: PacketMeta::default(),
        };
        if let Some(n) = self.try_deliver_local(remote, &send_buf)? {
            return Ok(n);
        }
        if let Err(e) = route_check(remote.addr) {
            return Err(e);
        }
        if let Some(route) = crate::net::routing::route_output(remote.addr).ok() {
            NET_INTERFACE.rebind_routed_udp(self.socket_handler, route.ifindex);
        }
        NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| {
                if !socket.can_send() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.send_slice(&send_buf, meta) {
                    Ok(()) => Ok(buf.len() as isize),
                    Err(SendError::Unaddressable) => Err(SyscallErr::ENOTCONN),
                    Err(SendError::BufferFull) => Err(SyscallErr::EAGAIN),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }
    fn last_recv_addr(&self) -> Option<Endpoint> {
        self.inner.lock().last_recv_addr.take().map(Endpoint::Ip)
    }

    fn socket_r_ready(&self) -> bool {
        NET_INTERFACE.poll();
        !self.inner.lock().rx_queue.is_empty()
    }

    fn socket_w_ready(&self) -> bool {
        NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| socket.can_send())
            .unwrap_or(false)
    }

    fn socket_hang_up(&self) -> bool {
        false
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.recv_waiters.wait_queue())
    }

    fn recv_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.send_waiters.wait_queue())
    }

    fn send_event_queue(&self) -> Option<&EventWaitQueue> {
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
        let socket_handler = NET_INTERFACE.add_routed_socket(InetProtocol::Udp, socket).unwrap();
        log::info!("[UdpSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        Self {
            inner: Mutex::new(UdpSocketInner {
                remote_endpoint: None,
                local_endpoint: None,
                rx_queue: VecDeque::new(),
                last_recv_addr: None,
                msg_more_buf: Vec::new(),
                recvbuf_size: MAX_BUFFER_SIZE,
                sendbuf_size: MAX_BUFFER_SIZE,
                reuse_addr: false,
                multicast_group_joined: false,
            }),
            socket_handler,
            bound: Mutex::new(BoundInner::new()),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
        }
    }
    pub fn bound_inner(&self) -> BoundInner {
        self.bound.lock().clone()
    }

    pub fn register_udp_socket(socket: &Arc<Self>) {
        let local = socket.inner.lock().local_endpoint;
        log::info!("[register_udp_socket] local={:?}", local);
        UDP_SOCKETS.lock().push(Arc::downgrade(socket));
    }

    fn local_source_endpoint(&self, remote: IpEndpoint) -> Option<IpEndpoint> {
        let local = self.inner.lock().local_endpoint?;
        if local.port == 0 {
            return None;
        }
        let addr = local.addr.unwrap_or_else(|| lookup_source_ip(remote.addr));
        Some(IpEndpoint::new(addr, local.port))
    }

    fn is_local_udp_destination(addr: IpAddress) -> bool {
        if addr.is_unspecified() {
            return true;
        }
        match addr {
            IpAddress::Ipv4(ip) => {
                ip.is_loopback() || crate::net::net_core::is_local_addr(ip)
            }
            IpAddress::Ipv6(_) => false,
        }
    }

    fn try_deliver_local(
        &self,
        remote: IpEndpoint,
        data: &[u8],
    ) -> Result<Option<isize>, SyscallErr> {
        if !Self::is_local_udp_destination(remote.addr) {
            return Ok(None);
        }
        let Some(src) = self.local_source_endpoint(remote) else {
            return Ok(None);
        };
        let Some(peer) = find_local_udp_recipient(remote, src) else {
            return Ok(None);
        };

        let mut peer_inner = peer.inner.lock();
        if peer_inner.rx_queue.len() >= peer_inner.recvbuf_size {
            return Err(SyscallErr::EAGAIN);
        }
        peer_inner.rx_queue.push_back((data.to_vec(), src));
        peer.recv_waiters
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
        Ok(Some(data.len() as isize))
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
        // NET_INTERFACE.udp_routed_socket(self.socket_handler, |socket| {
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

        let mut inner = self.inner.lock();
        if let Some((data, remote)) = inner.rx_queue.pop_front() {
            let copy_len = data.len().min(buf.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            // 对于未connect的socket，更新最近通信的对端(recvfrom需要)
            // if inner.remote_endpoint.is_none() {
            //     // 或者在 syscall recvfrom 里处理对端信息
            //     inner.remote_endpoint = Some(remote);
            // }
            log::debug!("[UdpSocket] read {} bytes from {:?}", copy_len, remote);
            return GeneralRet::Ok(copy_len);
        }
        GeneralRet::Err(SyscallErr::EAGAIN)
    }
}

// 新的分发函数：直接接收 NetInterfaceInner，避免重复获取锁导致死锁！
pub fn dispatch_udp_packets(sockets: &mut SocketSet) {
    let mut os_socks = UDP_SOCKETS.lock();

    // 顺便清理一下已经被 drop 掉的 socket
    os_socks.retain(|w| w.strong_count() > 0);

    log::debug!(
        "[dispatch_udp_packets] scanning {} os socks, {} smoltcp sockets",
        os_socks.len(),
        sockets.iter().count()
    );

    for (handle, socket) in sockets.iter_mut() {
        // 尝试把这个 socket 识别为 UDP 类型
        if let Some(udp_sock) = smoltcp::socket::udp::Socket::downcast_mut(socket) {
            // 只要这个底层缓冲区里有包，就全部抽干
            while udp_sock.can_recv() {
                // recv() 返回 Result<(&[u8], UdpMetadata), RecvError>
                match udp_sock.recv() {
                    Ok((data, meta)) => {
                        let remote = meta.endpoint;
                        let buf = data.to_vec();
                        log::debug!(
                            "[dispatch_udp_packets] recv {} bytes from {:?} on socket {}",
                            buf.len(),
                            remote,
                            handle
                        );
                        // 找到最匹配的 OS UdpSocket，放入它的 rx_queue
                        if let Some(os_sock) =
                            find_best_match(&os_socks, udp_sock.endpoint(), remote)
                        {
                            let mut inner = os_sock.inner.lock();
                            inner.rx_queue.push_back((buf, remote));
                            // 唤醒等待这个 socket 的任务
                            os_sock
                                .recv_waiters
                                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
                        } else {
                            log::warn!(
                                "[dispatch_udp_packets] no match for {:?}, local={:?}",
                                remote,
                                udp_sock.endpoint()
                            );
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "[dispatch_udp_packets] error receiving from socket {}: {:?}",
                            handle,
                            e
                        );
                        break; // 这个 socket 可能出问题了，先跳过它
                    }
                }
            }
        }
    }
}

// 寻找最匹配的 OS UdpSocket
fn find_local_udp_recipient(remote: IpEndpoint, src: IpEndpoint) -> Option<Arc<UdpSocket>> {
    let sockets = UDP_SOCKETS.lock();
    let mut best_match = None;
    let mut best_score = 0;

    for weak_sock in sockets.iter() {
        if let Some(sock) = weak_sock.upgrade() {
            let inner = sock.inner.lock();
            let Some(local) = inner.local_endpoint else {
                continue;
            };
            if local.port != remote.port {
                continue;
            }
            let addr_score = match local.addr {
                Some(addr) if addr == remote.addr => 2,
                Some(_) => continue,
                None => 1,
            };
            let peer_score = match inner.remote_endpoint {
                Some(peer) if peer == src => 2,
                Some(_) => continue,
                None => 1,
            };
            let score = addr_score + peer_score;
            if score > best_score {
                best_score = score;
                best_match = Some(sock.clone());
            }
        }
    }

    best_match
}

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
                log::debug!(
                    "[find_best_match] remote={:?} local_port={} remote_ep={:?} score={}",
                    remote,
                    local.port,
                    inner.remote_endpoint,
                    score
                );
                if score > best_score {
                    best_score = score;
                    best_match = Some(sock.clone());
                }
            }
        }
    }
    best_match
}
