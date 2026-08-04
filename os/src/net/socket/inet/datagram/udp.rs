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
use core::sync::atomic::{AtomicBool, Ordering};
use log::info;
use smoltcp::{
    iface::SocketSet,
    phy::PacketMeta,
    socket::{
        self,
        udp::{PacketMetadata, SendError, UdpMetadata},
        AnySocket,
    },
    wire::{IpAddress, IpEndpoint, IpListenEndpoint, IpVersion, Ipv4Address},
};

use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::net::socket::inet::common::port::{
    AddressFamily, AutoBindPurpose, BindIntent, PortReservation, TransportProtocol,
};
use crate::net::socket::inet::common::BoundInner;
use crate::net::{UDP_SOCKETS, UDP_SOCKETS_TO_REMOVE};
use crate::task::WaitQueue;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub struct UdpSocket {
    inner: Mutex<UdpSocketInner>,
    socket_handler: RouteSocketHandle,
    bound: Mutex<BoundInner>,
    bound_ifindex: Mutex<Option<u32>>,
    port_reservation: Mutex<Option<PortReservation>>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
    pub ip_version: IpVersion,
    ipv6_v6only: AtomicBool,
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
    ip_recv_err: bool,
    multicast_group_joined: bool,
    ipv6_checksum_offset: Option<u32>,
}

impl Socket for UdpSocket {
    /// 将 UDP socket 绑定到本地 IP 端点。
    ///
    /// # Semantics
    ///
    /// 规范化 IPv4-mapped IPv6 地址。`port=0` 已由 `PortRegistry` 在进入本方法前
    /// 选择并预留；这里通过 `NET_INTERFACE.udp_routed_socket()` 调用 socket.bind()。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁。`NET_INTERFACE.udp_routed_socket()` 通过路由句柄
    /// 执行短闭包——闭包内不持锁、不做 I/O。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：`endpoint` 不是 `Endpoint::Ip`
    /// - `EAFNOSUPPORT`：地址族不匹配
    /// - `EAGAIN`：poll 后绑定失败（smoltcp 接口状态不匹配）
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let ep = IpEndpoint::new(self.normalize_ipv4_mapped(ep.addr), ep.port);
        if !ep.addr.is_unspecified() && !self.addr_family_matches(ep.addr) {
            return Err(SyscallErr::EAFNOSUPPORT);
        }
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
        // PortRegistry 已在 N0 为 port=0 选择并保留端口；这里绝不能重新分配。
        let bind_addr = addr;
        NET_INTERFACE.request_poll();
        NET_INTERFACE
            .udp_routed_socket(self.socket_handler, |socket| {
                socket.bind(bind_addr).ok().ok_or(SyscallErr::EINVAL)
            })
            .ok_or(SyscallErr::EAGAIN)??;
        self.inner.lock().local_endpoint = Some(bind_addr);
        NET_INTERFACE.request_poll();
        let ifindex = crate::net::net_core::ifindex_for_local_addr(bind_addr.addr);
        self.bound
            .lock()
            .bind(self.socket_handler, ifindex, bind_addr.addr, bind_addr.port);
        log::debug!(
            "udp_bind: addr={:?} port={} ifindex={}",
            bind_addr.addr,
            bind_addr.port,
            ifindex
        );
        Ok(0)
    }

    fn snapshot_bind_intent(&self, endpoint: &Endpoint) -> Result<BindIntent, SyscallErr> {
        let Endpoint::Ip(endpoint) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let inner = self.inner.lock();
        if inner.local_endpoint.is_some() {
            return Err(SyscallErr::EINVAL);
        }
        let reuse_addr = inner.reuse_addr;
        drop(inner);
        let address = self.normalize_ipv4_mapped(endpoint.addr);
        if !address.is_unspecified() && !self.addr_family_matches(address) {
            return Err(SyscallErr::EAFNOSUPPORT);
        }
        let family = match self.ip_version {
            IpVersion::Ipv4 => AddressFamily::Ipv4,
            IpVersion::Ipv6 => AddressFamily::Ipv6,
        };
        Ok(BindIntent::inet(
            TransportProtocol::Udp,
            family,
            (!address.is_unspecified()).then_some(address),
            endpoint.port,
            *self.bound_ifindex.lock(),
            reuse_addr,
            self.ipv6_v6only.load(Ordering::Acquire),
        ))
    }

    fn install_port_reservation(&self, reservation: PortReservation) {
        *self.port_reservation.lock() = Some(reservation);
    }

    fn auto_bind_endpoint(
        &self,
        peer: Option<&Endpoint>,
        purpose: AutoBindPurpose,
    ) -> Result<Option<Endpoint>, SyscallErr> {
        let remote = {
            let inner = self.inner.lock();
            if inner.local_endpoint.is_some() {
                return Ok(None);
            }
            inner.remote_endpoint
        };
        let peer = match purpose {
            AutoBindPurpose::Connect | AutoBindPurpose::Send => peer
                .and_then(|endpoint| match endpoint {
                    Endpoint::Ip(endpoint) => Some(*endpoint),
                    _ => None,
                })
                .or(remote),
            AutoBindPurpose::Listen => return Ok(None),
        };
        let Some(peer) = peer else {
            return Ok(None);
        };
        let peer_addr = self.normalize_ipv4_mapped(peer.addr);
        let route_addr = if peer_addr.is_unspecified() {
            match peer_addr {
                IpAddress::Ipv4(_) => IpAddress::v4(127, 0, 0, 1),
                IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1),
            }
        } else {
            peer_addr
        };
        Ok(Some(Endpoint::Ip(IpEndpoint::new(
            lookup_source_ip(route_addr),
            0,
        ))))
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 设置 UDP socket 的远程目标地址。
    ///
    /// # Semantics
    ///
    /// syscall 层先经 `PortManager::ensure_auto_bound()` 绑定本地端点，再存储
    /// `remote_endpoint`。`unspecified` 远程地址映射为 127.0.0.1 / ::1。
    ///
    /// # Locking
    ///
    /// 只短暂获取 `self.inner` 锁；PortRegistry 和 DeviceStack 都不在此路径嵌套。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：`endpoint` 不是 `Endpoint::Ip`
    /// - `EAFNOSUPPORT`：地址族不匹配
    /// - `EAGAIN`：poll 后 smoltcp 绑定时接口状态不匹配
    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let ep = IpEndpoint::new(self.normalize_ipv4_mapped(ep.addr), ep.port);
        if !self.addr_family_matches(ep.addr) {
            return Err(SyscallErr::EAFNOSUPPORT);
        }
        let remote_endpoint = if ep.addr.is_unspecified() {
            let loopback_addr = match ep.addr {
                IpAddress::Ipv4(_) => IpAddress::v4(127, 0, 0, 1),
                IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1),
            };
            IpEndpoint::new(loopback_addr, ep.port)
        } else {
            ep
        };
        log::info!("[Udp::connect] connect to {:?}", remote_endpoint);
        let local_ep = self
            .inner
            .lock()
            .local_endpoint
            .ok_or(SyscallErr::EINVAL)?;
        {
            let mut inner = self.inner.lock();
            inner.remote_endpoint = Some(remote_endpoint);
        }
        let ifindex = crate::net::net_core::ifindex_for_local_addr(local_ep.addr);
        log::debug!(
            "udp_connect: remote={:?} ifindex={}",
            remote_endpoint,
            ifindex
        );
        NET_INTERFACE.request_poll();
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
        // 状态查询需要观察本次调用前已抵达的 worker 结果；等待发生在任何 socket/N2
        // 锁之外，不把异步 request 当作即时 smoltcp 进度。
        let ticket = NET_INTERFACE.request_poll_ticket();
        let _ = NET_INTERFACE.wait_poll_completion(ticket);
        let local: Option<IpListenEndpoint> =
            NET_INTERFACE.udp_routed_socket(self.socket_handler, |socket| socket.endpoint());
        NET_INTERFACE.request_poll();
        local.map(|ep| {
            let addr = ep.addr.unwrap_or_else(|| match self.ip_version {
                IpVersion::Ipv4 => IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
                IpVersion::Ipv6 => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED),
            });
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

    fn ip_recv_err(&self) -> Result<bool, SyscallErr> {
        Ok(self.inner.lock().ip_recv_err)
    }

    fn set_ip_recv_err(&self, enabled: bool) -> SyscallRet {
        self.inner.lock().ip_recv_err = enabled;
        Ok(0)
    }

    fn set_bind_to_device(&self, ifname: &str) -> SyscallRet {
        if ifname.is_empty() {
            *self.bound_ifindex.lock() = None;
            log::info!("[UdpSocket] unbound from device");
            return Ok(0);
        }
        let ns = crate::net::net_core::current_netns();
        let list = ns.device_list.lock();
        let iface = list.values().find(|d| d.iface_name() == ifname);
        match iface {
            Some(iface) => {
                *self.bound_ifindex.lock() = Some(iface.nic_id() as u32);
                log::info!(
                    "[UdpSocket] bound to device {} (ifindex={})",
                    ifname,
                    iface.nic_id()
                );
                Ok(0)
            }
            None => Err(SyscallErr::ENODEV),
        }
    }

    fn set_ipv6_checksum(&self, offset: u32) -> SyscallRet {
        self.inner.lock().ipv6_checksum_offset = Some(offset);
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
        // TODO(udp-sendto): implement `send_to` for UDP sockets.
        // Currently `try_send` handles the connected case; this path is unreachable
        // because `UdpSocket` is never used as a `Raw` socket without `connect`.
        // Exit condition: `UdpSocket` gains a code path that routes here without a connected endpoint.
        return Err(SyscallErr::EOPNOTSUPP);
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        self.try_recvmsg(buf).map(|(size, _)| size)
    }

    /// 非阻塞尝试发送 UDP 数据报（不 poll）。
    ///
    /// # Semantics
    ///
    /// 通过 `self.inner` 获取 `remote_endpoint`（必须已 `connect`，否则 `ENOTCONN`）。
    /// 支持 `MSG_MORE`：缓冲数据而非立即发送，非 `MSG_MORE` 则合并缓冲后一次性发送。
    ///
    /// 发送路径：本地回环直接通过 `try_deliver_local()` 推送到 peer 的 `rx_queue`；
    /// 否则通过 `NET_INTERFACE.udp_routed_socket()` 调用 smoltcp `socket.send_slice()`。
    ///
    /// **阻塞模型**：`try_xxx` 模式——不做 poll、不睡眠、不调度。调用者的
    /// `sys_sendto`/`sys_sendmsg` 负责 poll 和 WaitQueue 管理。
    ///
    /// **限制**：单次 `send_slice` 最多 `MAX_BUFFER_SIZE`（64KB），调用者通过
    /// `EMSGSIZE` 检查（`>65507`）确保不超过 UDP 有效载荷上限。
    ///
    /// # Errors
    ///
    /// - `EMSGSIZE`：`buf.len() > 65507`（UDP 最大有效载荷）
    /// - `ENOTCONN`：无 `remote_endpoint`
    /// - `EAGAIN`：发送缓冲满（`BufferFull`）
    /// - `ENOBUFS`：smoltcp 内部分配失败
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

    /// 非阻塞尝试接收 UDP 数据报及其源地址。
    ///
    /// # Semantics
    ///
    /// 从 `self.inner.rx_queue` 弹出最早的数据报（`VecDeque` 前端）。
    /// 将最多 `buf.len()` 字节数据复制到 `buf` 中，返回 `(本地读取长度, Some(Endpoint::Ip(remote)))`。
    /// 队列由 CPU0 poll worker 的 `dispatch_udp_packets()` 填充。
    ///
    /// **阻塞模型**：`try_xxx` 模式——仅消费已有数据，不等待。队列为空时返回
    /// `EAGAIN`，调用者通过 `recv_wait_queue` 进入阻塞等待。
    ///
    /// **截断行为**：若数据报 > `buf.len()`，超出的数据丢弃（无 `MSG_TRUNC` 通知）。
    ///
    /// # Errors
    ///
    /// - `EAGAIN`：接收队列为空
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

    /// 非阻塞尝试发送 UDP 数据报到指定目标。
    ///
    /// # Semantics
    ///
    /// 与 `try_send` 类似，但支持显式 `dest: Option<Endpoint>` 参数：
    /// - `Some(Endpoint::Ip(ep))`：发送到该地址
    /// - `None`：回退到 `self.try_send()`（使用 `remote_endpoint`）
    ///
    /// 发送前做路由检查（`route_check`）和 `SO_BINDTODEVICE` 检查。若目标需要
    /// 特定的输出接口，重新绑定 smoltcp handler（`rebind_routed_udp`）。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁两次：一次读取 `remote_endpoint`，一次读取和消费
    /// `msg_more_buf`。
    ///
    /// # Errors
    ///
    /// - `EMSGSIZE`：数据报过大
    /// - `EINVAL`：`dest` 不是 `Endpoint::Ip`
    /// - `ENOTCONN`：`dest==None` 且无 `remote_endpoint`
    /// - `EAFNOSUPPORT`：地址族不匹配
    /// - `EAGAIN`：发送缓冲满
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
            let remote = IpEndpoint::new(self.normalize_ipv4_mapped(remote.addr), remote.port);
            if !self.addr_family_matches(remote.addr) {
                return Err(SyscallErr::EAFNOSUPPORT);
            }
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
        let bound = *self.bound_ifindex.lock();
        let target_ifindex = bound.or_else(|| {
            crate::net::routing::route_output(remote.addr)
                .ok()
                .map(|r| r.ifindex)
        });
        if let Some(ifidx) = target_ifindex {
            NET_INTERFACE.rebind_routed_udp(self.socket_handler, ifidx);
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
        // epoll/poll 的 state scan 只能读取已发布队列状态；poll worker 会在锁外
        // 分发 UDP 包并通知 recv_waiters，不能从这里重入 smoltcp。
        NET_INTERFACE.request_poll();
        self.recv_ready()
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

    fn set_ipv6_v6only(&self, enabled: bool) -> SyscallRet {
        if self.ip_version != IpVersion::Ipv6 {
            return Err(SyscallErr::ENOPROTOOPT);
        }
        self.ipv6_v6only.store(enabled, Ordering::Release);
        Ok(0)
    }
}

impl UdpSocket {
    pub fn new(ver: IpVersion) -> Self {
        let tx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1024],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::udp::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 1024],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = socket::udp::Socket::new(rx_buf, tx_buf);
        let socket_handler = NET_INTERFACE
            .add_routed_socket(InetProtocol::Udp, socket)
            .unwrap();
        log::info!("[UdpSocket::new] new {}", socket_handler);
        NET_INTERFACE.request_poll();
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
                ip_recv_err: false,
                multicast_group_joined: false,
                ipv6_checksum_offset: None,
            }),
            socket_handler,
            bound: Mutex::new(BoundInner::new()),
            bound_ifindex: Mutex::new(None),
            port_reservation: Mutex::new(None),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            ip_version: ver,
            ipv6_v6only: AtomicBool::new(false),
        }
    }
    pub fn bound_inner(&self) -> BoundInner {
        self.bound.lock().clone()
    }

    fn addr_family_matches(&self, addr: IpAddress) -> bool {
        match self.ip_version {
            IpVersion::Ipv4 => matches!(addr, IpAddress::Ipv4(_)),
            IpVersion::Ipv6 => {
                if !self.ipv6_v6only.load(Ordering::Acquire) && matches!(addr, IpAddress::Ipv4(_)) {
                    return true;
                }
                matches!(addr, IpAddress::Ipv6(_))
            }
        }
    }

    fn normalize_ipv4_mapped(&self, addr: IpAddress) -> IpAddress {
        if let IpAddress::Ipv6(v6) = addr {
            if let Some(ipv4) = v6.as_ipv4() {
                return IpAddress::Ipv4(ipv4);
            }
        }
        addr
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
            IpAddress::Ipv4(ip) => ip.is_loopback() || crate::net::net_core::is_local_addr(ip),
            IpAddress::Ipv6(ip) => ip.is_loopback(),
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
        let reservation = self.port_reservation.lock().take();
        if let Some(reservation) = reservation {
            // 释放与本 socket Weak 身份相同的一个 owner；reuse peer 不受影响。
            reservation.release();
        }
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
        let ticket = NET_INTERFACE.request_poll_ticket();
        let _ = NET_INTERFACE.wait_poll_completion(ticket);

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

/// 在 DeviceStack 锁内从 smoltcp 接收队列转移出的内核所有数据包。
pub(crate) struct UdpPacketDelivery {
    local: IpListenEndpoint,
    remote: IpEndpoint,
    payload: Vec<u8>,
}

/// 只接触 `SocketSet`，因此可以安全地在 DeviceStack 临界区内调用。
pub(crate) fn drain_udp_packets(sockets: &mut SocketSet) -> Vec<UdpPacketDelivery> {
    let mut deliveries = Vec::new();
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
                        deliveries.push(UdpPacketDelivery {
                            local: udp_sock.endpoint(),
                            remote,
                            payload: buf,
                        });
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
    deliveries
}

/// 在 DeviceStack 释放后分发到 OS socket；不能在 smoltcp 锁内取得 socket lifecycle
/// 或 EventWaitQueue 锁，否则会形成 N2 -> N1/N3 反向嵌套。
pub(crate) fn dispatch_udp_packets(deliveries: Vec<UdpPacketDelivery>) {
    let sockets: Vec<Arc<UdpSocket>> = {
        let mut registered = UDP_SOCKETS.lock();
        registered.retain(|socket| socket.strong_count() > 0);
        registered.iter().filter_map(Weak::upgrade).collect()
    };
    for delivery in deliveries {
        let Some(socket) = find_best_match(&sockets, delivery.local, delivery.remote) else {
            log::warn!(
                "[dispatch_udp_packets] no match for {:?}, local={:?}",
                delivery.remote,
                delivery.local
            );
            continue;
        };
        {
            let mut inner = socket.inner.lock();
            inner.rx_queue.push_back((delivery.payload, delivery.remote));
        }
        socket
            .recv_waiters
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
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
    sockets: &[Arc<UdpSocket>],
    local: IpListenEndpoint,
    remote: IpEndpoint,
) -> Option<Arc<UdpSocket>> {
    let mut best_match = None;
    let mut best_score = 0;

    for sock in sockets {
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
                    best_match = Some(Arc::clone(sock));
                }
            }
    }
    best_match
}
