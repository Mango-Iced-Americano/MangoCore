use crate::net::routing::{InetProtocol, RouteSocketHandle};
use crate::net::syscall::common::MsgFlags;
use crate::net::{
    config::NET_INTERFACE, Endpoint, Mutex, Socket, MAX_BUFFER_SIZE, RAW_SOCKETS, SHUT_WR,
};
use crate::fs::vfs::event::EventWaitQueue;
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use log::info;
use smoltcp::{
    socket::{self, raw, raw::PacketMetadata},
    wire::{IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion},
};

pub struct RawSocket {
    inner: Mutex<RawSocketInner>,
    socket_handler: RouteSocketHandle,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}

#[allow(unused)]
struct RawSocketInner {
    local_endpoint: Option<IpListenEndpoint>,
    remote_endpoint: Option<IpEndpoint>,
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
    recvbuf_size: usize,
    sendbuf_size: usize,
    bound_ifindex: Option<u32>,
}

impl Socket for RawSocket {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        match endpoint {
            Endpoint::Ip(ep) => {
                let listen_ep = IpListenEndpoint {
                    addr: Some(ep.addr),
                    port: 0,
                };
                self.inner.lock().local_endpoint = Some(listen_ep);
                Ok(0)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        match endpoint {
            Endpoint::Ip(ep) => {
                self.inner.lock().remote_endpoint = Some(*ep);
                Ok(0)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP) // Not implemented for raw sockets
    }

    fn socket_type(&self) -> crate::net::PSOCK {
        crate::net::PSOCK::Raw
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size;
    }

    fn set_bind_to_device(&self, ifname: &str) -> SyscallRet {
        let ns = crate::net::net_core::current_netns();
        let list = ns.device_list.lock();
        let iface = list.values().find(|d| d.iface_name() == ifname);
        match iface {
            Some(iface) => {
                self.inner.lock().bound_ifindex = Some(iface.nic_id() as u32);
                log::info!("[RawSocket] bound to device {} (ifindex={})", ifname, iface.nic_id());
                Ok(0)
            }
            None => Err(SyscallErr::ENODEV),
        }
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        self.inner.lock().local_endpoint.and_then(|ep| {
            ep.addr.map(|addr| Endpoint::Ip(IpEndpoint::new(addr, 0)))
        })
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        self.inner.lock().remote_endpoint.map(Endpoint::Ip)
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        info!("[RawSocket::shutdown] how {}", how);
        todo!()
    }

    fn send_to(&self, user_buf: &[u8], dest: Endpoint) -> SyscallRet {
        let Endpoint::Ip(dest_addr) = dest else {
            return Err(SyscallErr::EINVAL);
        };
        let (version, protocol) = {
            let inner = self.inner.lock();
            (inner.ip_version, inner.ip_protocol)
        };
        match version {
            IpVersion::Ipv4 => {
                let target_ip = match dest_addr.addr {
                    smoltcp::wire::IpAddress::Ipv4(ip) => ip,
                    _ => return Err(SyscallErr::EINVAL),
                };
                let mut packet_buf = vec![0u8; 20 + user_buf.len()];

                log::info!("[RawSocketsendto] make ipv4 head...");
                //封装IP头
                let mut ip_pkg = smoltcp::wire::Ipv4Packet::new_unchecked(&mut packet_buf);
                ip_pkg.set_version(4);
                ip_pkg.set_header_len(20);
                ip_pkg.set_total_len((20 + user_buf.len()) as u16);
                ip_pkg.set_next_header(protocol); // 使用刚才解锁拿到的 protocol
                ip_pkg.set_hop_limit(64);
                ip_pkg.set_dst_addr(target_ip);

                // Resolve output interface: prefer SO_BINDTODEVICE, fall back to route lookup
                let target_ifindex = {
                    let bound = self.inner.lock().bound_ifindex;
                    bound.or_else(|| {
                        crate::net::routing::route_output(smoltcp::wire::IpAddress::Ipv4(target_ip))
                            .ok()
                            .map(|r| r.ifindex)
                    })
                };

                if let Some(ifidx) = target_ifindex {
                    NET_INTERFACE.rebind_routed_raw(self.socket_handler, ifidx);
                }

                // Source IP from the OUTPUT interface, not from destination-based lookup
                let src_addr = target_ifindex
                    .and_then(|ifidx| {
                        let ns = crate::net::net_core::current_netns();
                        let list = ns.device_list.lock();
                        list.values()
                            .find(|iface| iface.nic_id() as u32 == ifidx)
                            .and_then(|iface| iface.ip_addrs().first().map(|c| c.address()))
                    })
                    .and_then(|addr| match addr {
                        smoltcp::wire::IpAddress::Ipv4(a) => Some(a),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        // Fallback: use route-based source lookup
                        match crate::net::config::lookup_source_ip(
                            smoltcp::wire::IpAddress::Ipv4(target_ip),
                        ) {
                            smoltcp::wire::IpAddress::Ipv4(addr) => addr,
                            _ => smoltcp::wire::Ipv4Address::UNSPECIFIED,
                        }
                    });
                ip_pkg.set_src_addr(src_addr);

                ip_pkg.payload_mut().copy_from_slice(user_buf);
                ip_pkg.fill_checksum();

                NET_INTERFACE.poll();
                let ret = NET_INTERFACE
                    .raw_routed_socket(self.socket_handler, |socket| {
                        log::info!(
                            "[RawSocket] Sending {} bytes to {}",
                            user_buf.len(),
                            target_ip
                        );
                        match socket.send_slice(ip_pkg.into_inner()) {
                            Ok(_) => Ok(user_buf.len()),
                            Err(_) => Err(SyscallErr::ENOBUFS),
                        }
                    })
                    .ok_or(SyscallErr::EAGAIN)?;
                // Poll twice: first to flush TX from our stack to peer's rx_queue,
                // second to process the peer's reply back to our rx_queue.
                NET_INTERFACE.poll();
                NET_INTERFACE.poll();
                ret
            }
            IpVersion::Ipv6 => {
                let target_ip = match dest_addr.addr {
                    smoltcp::wire::IpAddress::Ipv6(ip) => ip,
                    _ => return Err(SyscallErr::EINVAL),
                };
                let mut packet_buf = vec![0u8; 40 + user_buf.len()];

                log::info!("[RawSocketsendto] make ipv6 head...");
                let mut ip_pkg = smoltcp::wire::Ipv6Packet::new_unchecked(&mut packet_buf);
                ip_pkg.set_version(6);
                ip_pkg.set_traffic_class(0);
                ip_pkg.set_flow_label(0);
                ip_pkg.set_payload_len(user_buf.len() as u16);
                ip_pkg.set_next_header(protocol);
                ip_pkg.set_hop_limit(64);
                ip_pkg.set_dst_addr(target_ip);

                // Resolve output interface: prefer SO_BINDTODEVICE, fall back to route lookup
                let target_ifindex = {
                    let bound = self.inner.lock().bound_ifindex;
                    bound.or_else(|| {
                        crate::net::routing::route_output(
                            smoltcp::wire::IpAddress::Ipv6(target_ip),
                        )
                        .ok()
                        .map(|r| r.ifindex)
                    })
                };

                if let Some(ifidx) = target_ifindex {
                    NET_INTERFACE.rebind_routed_raw(self.socket_handler, ifidx);
                }

                // Source IP from the OUTPUT interface: pick first non-unspecified IPv6 address
                let src_addr = target_ifindex
                    .and_then(|ifidx| {
                        let ns = crate::net::net_core::current_netns();
                        let list = ns.device_list.lock();
                        list.values()
                            .find(|iface| iface.nic_id() as u32 == ifidx)
                            .and_then(|iface| {
                                iface.ip_addrs().iter().find_map(|c| match c {
                                    smoltcp::wire::IpCidr::Ipv6(cidr) => {
                                        let addr = cidr.address();
                                        if !addr.is_unspecified() {
                                            Some(addr)
                                        } else {
                                            None
                                        }
                                    }
                                    _ => None,
                                })
                            })
                    })
                    .unwrap_or_else(|| {
                        // Fallback: use route-based source lookup
                        match crate::net::config::lookup_source_ip(
                            smoltcp::wire::IpAddress::Ipv6(target_ip),
                        ) {
                            smoltcp::wire::IpAddress::Ipv6(addr) => addr,
                            _ => smoltcp::wire::Ipv6Address::UNSPECIFIED,
                        }
                    });
                ip_pkg.set_src_addr(src_addr);

                ip_pkg.payload_mut().copy_from_slice(user_buf);

                NET_INTERFACE.poll();
                let ret = NET_INTERFACE
                    .raw_routed_socket(self.socket_handler, |socket| {
                        log::info!(
                            "[RawSocket] Sending {} bytes to {}",
                            user_buf.len(),
                            target_ip
                        );
                        match socket.send_slice(ip_pkg.into_inner()) {
                            Ok(_) => Ok(user_buf.len()),
                            Err(_) => Err(SyscallErr::ENOBUFS),
                        }
                    })
                    .ok_or(SyscallErr::EAGAIN)?;
                NET_INTERFACE.poll();
                NET_INTERFACE.poll();
                ret
            }
        }
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        let n = self.try_recv(buf)?;
        let ep = self.inner.lock().remote_endpoint.map(Endpoint::Ip);
        Ok((n, ep))
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        // 不调用 poll，只做一次尝试
        NET_INTERFACE
            .raw_routed_socket(self.socket_handler, |socket| {
                if !socket.can_recv() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.recv_slice(buf) {
                    Ok(nbytes) => {
                        let ip_version = self.inner.lock().ip_version;
                        match ip_version {
                            IpVersion::Ipv4 => {
                                let packet =
                                    smoltcp::wire::Ipv4Packet::new_unchecked(&buf[..nbytes]);
                                let src_addr = packet.src_addr();
                                self.inner.lock().remote_endpoint =
                                    Some(IpEndpoint::new(src_addr.into(), 0));
                            }
                            IpVersion::Ipv6 => {
                                let packet =
                                    smoltcp::wire::Ipv6Packet::new_unchecked(&buf[..nbytes]);
                                let src_addr = packet.src_addr();
                                self.inner.lock().remote_endpoint =
                                    Some(IpEndpoint::new(src_addr.into_address(), 0));
                            }
                        }
                        Ok(nbytes as isize)
                    }
                    Err(_) => Err(SyscallErr::ENOTCONN),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn try_sendmsg(
        &self,
        buf: &[u8],
        dest: Option<Endpoint>,
        flags: MsgFlags,
    ) -> Result<isize, SyscallErr> {
        match dest {
            Some(Endpoint::Ip(ep)) => self
                .send_to(buf, Endpoint::Ip(ep))
                .map(|n| n as isize),
            Some(_) => Err(SyscallErr::EINVAL),
            None => self.try_send(buf, flags),
        }
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let remote = self.inner.lock().remote_endpoint;
        match remote {
            Some(ep) => {
                // Connected raw socket: add IP header and route via send_to
                match self.send_to(buf, Endpoint::Ip(ep)) {
                    Ok(n) => Ok(n as isize),
                    Err(e) => Err(e),
                }
            }
            None => {
                // Unconnected: send raw bytes (IP_HDRINCL mode)
                NET_INTERFACE
                    .raw_routed_socket(self.socket_handler, |socket| {
                        if !socket.can_send() {
                            return Err(SyscallErr::EAGAIN);
                        }
                        match socket.send_slice(buf) {
                            Ok(()) => Ok(buf.len() as isize),
                            Err(_) => Err(SyscallErr::ENOBUFS),
                        }
                    })
                    .unwrap_or(Err(SyscallErr::EAGAIN))
            }
        }
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
        NET_INTERFACE
            .raw_routed_socket(self.socket_handler, |socket| socket.can_recv())
            .unwrap_or(false)
    }

    fn send_ready(&self) -> bool {
        NET_INTERFACE
            .raw_routed_socket(self.socket_handler, |socket| socket.can_send())
            .unwrap_or(false)
    }
}

impl RawSocket {
    pub fn new(protocol: u32) -> Self {
        let tx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = raw::Socket::new(
            smoltcp::wire::IpVersion::Ipv4,
            smoltcp::wire::IpProtocol::from(protocol as u8),
            rx_buf,
            tx_buf,
        );
        let socket_handler = NET_INTERFACE.add_routed_socket(InetProtocol::Raw, socket).unwrap();
        log::info!("[RawSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        let inner = RawSocketInner {
            local_endpoint: None,
            remote_endpoint: None,
            ip_version: IpVersion::Ipv4,
            ip_protocol: IpProtocol::from(protocol as u8),
            recvbuf_size: MAX_BUFFER_SIZE,
            sendbuf_size: MAX_BUFFER_SIZE,
            bound_ifindex: None,
        };

        Self {
            inner: Mutex::new(inner),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            socket_handler,
        }
    }

    pub fn register_raw_socket(socket: &Arc<Self>) {
        crate::net::RAW_SOCKETS
            .lock()
            .push((socket.socket_handler, Arc::downgrade(socket)));
    }
}

impl Drop for RawSocket {
    fn drop(&mut self) {
        log::info!("[RawSocket::drop] removing handle {}", self.socket_handler);
        crate::net::RAW_SOCKETS
            .lock()
            .retain(|(h, _)| *h != self.socket_handler);
        crate::net::config::NET_INTERFACE.remove_routed(self.socket_handler);
    }
}
