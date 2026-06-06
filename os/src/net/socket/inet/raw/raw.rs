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
    /// Primary handler at index 0; subsequent handlers cover other stacks (lo, veth).
    socket_handlers: Vec<RouteSocketHandle>,
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
    ipv6_checksum_offset: Option<u32>,
    /// 256-bit bitmap: bit=1 means BLOCK (match Linux ICMP6_FILTER semantics)
    icmp6_filter: [u32; 8],
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

    fn set_icmp6_filter(&self, filter: [u32; 8]) -> SyscallRet {
        self.inner.lock().icmp6_filter = filter;
        Ok(0)
    }

    fn set_ipv6_checksum(&self, offset: u32) -> SyscallRet {
        if offset & 1 != 0 {
            return Err(SyscallErr::EINVAL);
        }
        self.inner.lock().ipv6_checksum_offset = Some(offset);
        Ok(0)
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
                    NET_INTERFACE.rebind_routed_raw(self.socket_handlers[0], ifidx, version, protocol);
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
                let ret =                 NET_INTERFACE
                    .raw_routed_socket(self.socket_handlers[0], |socket| {
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
                    NET_INTERFACE.rebind_routed_raw(self.socket_handlers[0], ifidx, version, protocol);
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

                let csum_offset = self.inner.lock().ipv6_checksum_offset;
                if let Some(off) = csum_offset {
                    let off = off as usize;
                    if off + 2 <= user_buf.len() {
                        let payload = ip_pkg.payload_mut();
                        payload[off] = 0;
                        payload[off + 1] = 0;
                        let csum = ipv6_pseudo_header_checksum(
                            &src_addr.0,
                            &target_ip.0,
                            user_buf.len() as u32,
                            u8::from(protocol),
                            payload,
                        );
                        payload[off] = (csum >> 8) as u8;
                        payload[off + 1] = (csum & 0xFF) as u8;
                    }
                }

                NET_INTERFACE.poll();
                let ret = NET_INTERFACE
                    .raw_routed_socket(self.socket_handlers[0], |socket| {
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
        let ip_version = self.inner.lock().ip_version;
        let icmp6_filter = self.inner.lock().icmp6_filter;

        for &handler in &self.socket_handlers {
            let result = NET_INTERFACE.raw_routed_socket(handler, |socket| {
                loop {
                    if !socket.can_recv() {
                        return Err(SyscallErr::EAGAIN);
                    }
                    match socket.recv_slice(buf) {
                        Ok(nbytes) => {
                            if ip_version == IpVersion::Ipv6 && nbytes > 40 {
                                let icmp_type = buf[40] as usize;
                                if icmp_type < 256 {
                                    let word_idx = icmp_type / 32;
                                    let bit_idx = icmp_type % 32;
                                    if (icmp6_filter[word_idx] & (1u32 << bit_idx)) != 0 {
                                        continue;
                                    }
                                }
                            }
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
                                    let payload_len = nbytes - 40;
                                    buf.copy_within(40..nbytes, 0);
                                    return Ok(payload_len as isize);
                                }
                            }
                            return Ok(nbytes as isize);
                        }
                        Err(_) => return Err(SyscallErr::ENOTCONN),
                    }
                }
            });
            match result {
                Some(Ok(n)) => return Ok(n),
                Some(Err(SyscallErr::EAGAIN)) => continue,
                Some(Err(e)) => return Err(e),
                None => continue,
            }
        }
        Err(SyscallErr::EAGAIN)
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
                    .raw_routed_socket(self.socket_handlers[0], |socket| {
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
        for &handler in &self.socket_handlers {
            if NET_INTERFACE
                .raw_routed_socket(handler, |socket| socket.can_recv())
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }

    fn socket_r_ready(&self) -> bool {
        for &handler in &self.socket_handlers {
            if NET_INTERFACE
                .raw_routed_socket(handler, |socket| socket.can_recv())
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }

    fn send_ready(&self) -> bool {
        NET_INTERFACE
            .raw_routed_socket(self.socket_handlers[0], |socket| socket.can_send())
            .unwrap_or(false)
    }
}

impl RawSocket {
    pub fn new(protocol: u32, ip_version: IpVersion) -> Self {
        let ip_protocol = smoltcp::wire::IpProtocol::from(protocol as u8);
        let ifindexes = NET_INTERFACE.stack_ifindexes();
        let mut handlers = Vec::with_capacity(ifindexes.len().max(1));

        if ifindexes.is_empty() {
            let tx_buf = socket::raw::PacketBuffer::new(
                vec![PacketMetadata::EMPTY; 128],
                vec![0 as u8; MAX_BUFFER_SIZE],
            );
            let rx_buf = socket::raw::PacketBuffer::new(
                vec![PacketMetadata::EMPTY; 128],
                vec![0 as u8; MAX_BUFFER_SIZE],
            );
            let socket = raw::Socket::new(ip_version, ip_protocol, rx_buf, tx_buf);
            let handler = NET_INTERFACE
                .add_routed_socket(InetProtocol::Raw, socket)
                .unwrap();
            handlers.push(handler);
            log::info!(
                "[RawSocket::new] handler {} (fallback default iface) ver={:?}",
                handler,
                ip_version
            );
        } else {
            for &ifidx in &ifindexes {
                let tx_buf = socket::raw::PacketBuffer::new(
                    vec![PacketMetadata::EMPTY; 128],
                    vec![0 as u8; MAX_BUFFER_SIZE],
                );
                let rx_buf = socket::raw::PacketBuffer::new(
                    vec![PacketMetadata::EMPTY; 128],
                    vec![0 as u8; MAX_BUFFER_SIZE],
                );
                let socket = raw::Socket::new(ip_version, ip_protocol, rx_buf, tx_buf);
                let handler = NET_INTERFACE
                    .add_routed_socket_on(InetProtocol::Raw, socket, ifidx)
                    .unwrap();
                handlers.push(handler);
                log::info!(
                    "[RawSocket::new] handler {} on ifindex={} ver={:?}",
                    handler,
                    ifidx,
                    ip_version
                );
            }
        }

        NET_INTERFACE.poll();
        let inner = RawSocketInner {
            local_endpoint: None,
            remote_endpoint: None,
            ip_version,
            ip_protocol,
            recvbuf_size: MAX_BUFFER_SIZE,
            sendbuf_size: MAX_BUFFER_SIZE,
            bound_ifindex: None,
            ipv6_checksum_offset: None,
            icmp6_filter: [0u32; 8],
        };

        Self {
            inner: Mutex::new(inner),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            socket_handlers: handlers,
        }
    }

    pub fn register_raw_socket(socket: &Arc<Self>) {
        crate::net::RAW_SOCKETS
            .lock()
            .push((socket.socket_handlers[0], Arc::downgrade(socket)));
    }
}

/// Compute the IPv6 pseudo-header checksum (RFC 2460 §8.1).
/// Used by IPV6_CHECKSUM to insert checksums into the payload of raw IPv6 packets.
fn ipv6_pseudo_header_checksum(
    src_addr: &[u8; 16],
    dst_addr: &[u8; 16],
    payload_len: u32,
    next_header: u8,
    payload: &[u8],
) -> u16 {
    let mut sum: u32 = 0;

    for i in (0..16).step_by(2) {
        sum += u16::from_be_bytes([src_addr[i], src_addr[i + 1]]) as u32;
    }
    for i in (0..16).step_by(2) {
        sum += u16::from_be_bytes([dst_addr[i], dst_addr[i + 1]]) as u32;
    }

    sum += (payload_len >> 16) & 0xFFFF;
    sum += payload_len & 0xFFFF;

    sum += (next_header as u32) << 8;

    for i in (0..payload.len()).step_by(2) {
        let word = if i + 1 < payload.len() {
            u16::from_be_bytes([payload[i], payload[i + 1]]) as u32
        } else {
            (payload[i] as u32) << 8
        };
        sum += word;
    }

    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }

    !sum as u16
}

impl Drop for RawSocket {
    fn drop(&mut self) {
        log::info!("[RawSocket::drop] removing {} handles", self.socket_handlers.len());
        for &handler in &self.socket_handlers {
            crate::net::RAW_SOCKETS
                .lock()
                .retain(|(h, _)| *h != handler);
            crate::net::config::NET_INTERFACE.remove_routed(handler);
        }
    }
}
