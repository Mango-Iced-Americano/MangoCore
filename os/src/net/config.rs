use super::Mutex;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{NullNetDevice, RoutingDevice, SmoltcpDeviceAdapter};
use crate::net::routing::{InetProtocol, RouteSocketHandle, SocketBinding};
use crate::net::socket::inet::datagram::udp::dispatch_udp_packets;
use crate::net::socket::inet::stream::inner::tcp_state_code;
use crate::net::net_core;
use crate::net::{TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS_TO_REMOVE};
use crate::timer::current_time_duration;
use crate::trace_event;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium},
    socket::{dhcpv4, raw, tcp, udp, AnySocket},
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr},
};

pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub fn init() {
    // Initialize net_core first (registers lo and eth0 into IFACES).
    // Must happen before NET_INTERFACE.init() so that NetInterfaceInner::new()
    // can read IP addresses from net_core::IFACES.
    let has_nic = NET_DEVICE.lock().is_some();
    net_core::init();
    NET_INTERFACE.init();
    if has_nic {
        println!("[kernel] net interface initialized (RoutingDevice: lo + eth)");
    } else {
        println!("[kernel] net interface initialized (loopback only, no NIC)");
    }
}

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}

pub struct NetInterfaceInner<'a> {
    pub device: RoutingDevice,
    // pub device: SmoltcpDeviceAdapter,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}

impl<'a> NetInterfaceInner<'a> {
    fn resolve(&self, rh: RouteSocketHandle) -> Option<SocketHandle> {
        self.bindings.get(&rh).map(|b| b.handle)
    }

    fn new() -> Self {
        let (eth, hw_addr, has_real_nic) = match NET_DEVICE.lock().take() {
            Some(net_device) => {
                let mac = net_device.mac_address();
                (SmoltcpDeviceAdapter::new(net_device), EthernetAddress(mac), true)
            }
            None => {
                println!("[kernel] No net device, using null device (loopback only)");
                let null_dev = Arc::new(NullNetDevice);
                let null_mac = [0x02u8, 0, 0, 0, 0, 1];
                (SmoltcpDeviceAdapter::new(null_dev), EthernetAddress(null_mac), false)
            }
        };
        let lo = Loopback::new(Medium::Ip);
        let mut device = RoutingDevice::new(eth, lo);

        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let config = Config::new(HardwareAddress::Ethernet(hw_addr));
        let mut iface = Interface::new(config, &mut device, now);

        // Create SocketSet early for DHCP probe
        let mut sockets = SocketSet::new(vec![]);

        if has_real_nic {
            let mut dhcp_socket = dhcpv4::Socket::new();
            dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
                discover_timeout: Duration::from_secs(2),
                initial_request_timeout: Duration::from_secs(1),
                request_retries: 3,
                min_renew_timeout: Duration::from_secs(60),
                ..dhcpv4::RetryConfig::default()
            });
            let dhcp_handle = sockets.add(dhcp_socket);
            let deadline = Instant::from_millis(
                current_time_duration().as_millis() as i64 + 5000,
            );

            loop {
                let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
                iface.poll(timestamp, &mut device, &mut sockets);

                let event = sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
                match event {
                    Some(dhcpv4::Event::Configured(cfg)) => {
                        net_core::set_eth0_ipv4(IpCidr::Ipv4(cfg.address));
                        net_core::set_default_gateway(cfg.router);
                        log::info!(
                            "[net::config] DHCP: got IP {:?} gateway {:?}",
                            cfg.address,
                            cfg.router
                        );
                        break;
                    }
                    Some(dhcpv4::Event::Deconfigured) => {}
                    None => {}
                }

                if timestamp >= deadline {
                    log::info!("[net::config] DHCP timeout, continuing without IP");
                    break;
                }
            }
            sockets.remove(dhcp_handle);
        }

        // Source IP addresses from net_core registered interfaces
        let addrs_src: Vec<IpCidr> = {
            let ifaces = net_core::IFACES.lock();
            ifaces
                .iter()
                .flat_map(|dev| dev.ip_addrs.iter().copied())
                .collect()
        };
        iface.update_ip_addrs(|addrs| {
            addrs.clear();
            for cidr in &addrs_src {
                addrs.push(*cidr).unwrap();
            }
        });
        log::info!("[net::config] sourced addresses from net_core: {:?}", addrs_src);

        // Default route from net_core (set by DHCP probe if NIC present)
        if let Some(gw) = net_core::default_gateway() {
            iface.routes_mut().add_default_ipv4_route(gw).unwrap();
        }

        Self {
            device,
            iface,
            sockets,
            bindings: BTreeMap::new(),
            next_socket_id: 1,
        }
    }
}

impl<'a> NetInterface<'a> {
    pub fn init(&self) {
        self._init();
    }

    pub fn add_socket<T>(&self, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        self._add_socket(socket)
    }

    pub fn _init(&self) {
        *self.inner.lock() = Some(NetInterfaceInner::new());
    }
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn _add_socket<T>(&self, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        Some(self.inner.lock().as_mut()?.sockets.add(socket))
    }

    pub fn tcp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<tcp::Socket>(handler);
        Some(f(socket))
    }

    pub fn udp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<udp::Socket>(handler);
        Some(f(socket))
    }

    pub fn raw_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<raw::Socket>(handler);
        Some(f(socket))
    }

    pub fn inner_handler<T>(&self, f: impl FnOnce(&mut NetInterfaceInner<'a>) -> T) -> Option<T> {
        Some(f(self.inner.lock().as_mut()?))
    }

    /// 返回 (tcp_count, udp_count, raw_count, pending_remove)
    pub fn socket_stats(&self) -> (usize, usize, usize, usize) {
        let tcp = crate::net::TCP_SOCKETS.lock().len();
        let raw = crate::net::RAW_SOCKETS.lock().len();
        let pending = TCP_SOCKETS_TO_REMOVE.lock().len() + UDP_SOCKETS_TO_REMOVE.lock().len();
        // UDP: count via inner sockets (only if initialized)
        let udp = match self.inner.lock().as_ref() {
            Some(inner) => inner.sockets.iter().count().saturating_sub(tcp).saturating_sub(raw),
            None => 0,
        };
        (tcp, udp, raw, pending)
    }

    pub fn poll(&self) {
        if self.inner.lock().is_none() {
            return;
        }
        self.poll_once();
    }

    /// Non-blocking poll: skip if the inner lock is already held
    /// (e.g., a syscall handler is already polling).
    /// Safe for use in interrupt contexts — never spins.
    pub fn try_poll(&self) -> bool {
        let guard = self.inner.try_lock();
        match guard {
            Some(inner) if inner.is_some() => {
                drop(inner);
                self.poll_once();
                true
            }
            _ => false, // lock held by another context, or NetInterface not yet initialized
        }
    }
    fn poll_once(&self) -> bool {
        let mut progressed = false;
        self.inner_handler(|inner| {
            // Trace: dump all TCP socket states BEFORE poll
            for (handle, sock) in inner.sockets.iter() {
                if let smoltcp::socket::Socket::Tcp(tcp_sock) = sock {
                    let sc = tcp_state_code(&tcp_sock.state());
                    trace_event!(0xB035, handle.as_usize() as u64, sc, 0, 0, 0, 0);
                }
            }

            // 1. 先清理标记删除的 UDP sockets
            let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
            for rh in to_remove.drain(..) {
                if let Some(h) = inner.resolve(rh) {
                    inner.sockets.remove(h);
                }
                inner.bindings.remove(&rh);
            }
            drop(to_remove);

            // 2. 驱动协议栈
            let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
            progressed = inner
                .iface
                .poll(timestamp, &mut inner.device, &mut inner.sockets);

            // 3. 清理符合条件的 TCP sockets
            let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
            let pending = to_remove.len();
            let ready: Vec<RouteSocketHandle> = to_remove
                .iter()
                .filter(|&&rh| {
                    if let Some(h) = inner.resolve(rh) {
                        let socket = inner.sockets.get::<tcp::Socket>(h);
                        let state = socket.state();
                        let can_remove = state == tcp::State::Closed;
                        if !can_remove {
                            log::debug!(
                                "[NetInterface::poll_once] TCP handle {:?} not ready yet (state={:?}), deferring",
                                rh, state
                            );
                        }
                        can_remove
                    } else {
                        true // stale binding, remove
                    }
                })
                .copied()
                .collect();
            if !ready.is_empty() {
                log::info!(
                    "[NetInterface::poll_once] removing {} of {} pending TCP sockets",
                    ready.len(),
                    pending
                );
            }
            for &rh in &ready {
                if let Some(h) = inner.resolve(rh) {
                    inner.sockets.remove(h);
                    log::info!("[NetInterface::poll_once] TCP socket {:?} fully removed from SocketSet", rh);
                }
                inner.bindings.remove(&rh);
            }
            to_remove.retain(|rh| !ready.contains(rh));
            if to_remove.len() > 0 {
                log::debug!("[NetInterface::poll_once] {} TCP handles still pending removal", to_remove.len());
            }
            drop(to_remove);

            // 4. 分发 UDP 包（必须在每次 poll 后立刻做）
            log::debug!("[poll_once] about to dispatch_udp_packets");
            dispatch_udp_packets(inner);

            // Trace: dump all TCP socket states AFTER poll
            // for (handle, sock) in inner.sockets.iter() {
            //     if let smoltcp::socket::Socket::Tcp(tcp_sock) = sock {
            //         let sc = tcp_state_code(&tcp_sock.state());
            //         trace_event!(0xB035, handle.as_usize() as u64, sc, 1, 0, 0, 0);
            //     }
            // }
        });

        // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        if progressed {
            crate::net::wake_tcp_waiters();
            crate::net::wake_raw_waiters();
        }

        // Trace: 记录 poll 后仍在连接中的 TCP socket 数
        // {
        //     let sockets = TCP_SOCKETS.lock();
        //     trace_event!(0xB033, sockets.len() as u64, 0, 0, 0, 0, 0);
        // }
        // config.rs poll_once() 中，在 poll 调用后加：
        // trace_event!(0xB036, progressed as u64, 0, 0, 0, 0, 0); // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        progressed
    }

    pub fn poll_until_quiescent(&self) {
        while self.try_poll() {
            // 继续推进，直到没有数据可处理
            crate::task::try_yield(); // 可选：避免占着 CPU 不放
        }
    }
    pub fn _poll(&self) {
        log::trace!("[NetInterface::poll] poll...");
        self.inner_handler(|inner| {
            {
                // 使用 drain(..) 一次性清空队列并取出所有元素
                let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
                for rh in to_remove.drain(..) {
                    if let Some(h) = inner.resolve(rh) {
                        inner.sockets.remove(h);
                        log::info!(
                            "[NetInterface] Successfully removed underlying socket {:?}",
                            rh
                        );
                    }
                    inner.bindings.remove(&rh);
                }
            }
            // poll 必须在删除 TCP socket 之前，这样 drop 时 close() 触发的
            // FIN/ACK 握手能在这个 poll 周期内完成（loopback 下一次 poll 即可完成）
            inner.iface.poll(
                Instant::from_millis(current_time_duration().as_millis() as i64),
                &mut inner.device,
                &mut inner.sockets,
            );
            {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                let ready: Vec<RouteSocketHandle> = to_remove
                    .iter()
                    .filter(|&&rh| {
                        if let Some(h) = inner.resolve(rh) {
                            let socket = inner.sockets.get::<tcp::Socket>(h);
                            socket.state() == tcp::State::Closed
                                || socket.state() == tcp::State::TimeWait
                        } else {
                            true
                        }
                    })
                    .copied()
                    .collect();
                for &rh in &ready {
                    if let Some(h) = inner.resolve(rh) {
                        inner.sockets.remove(h);
                        log::info!(
                            "[NetInterface] Successfully removed underlying TCP socket {:?}",
                            rh
                        );
                    }
                    inner.bindings.remove(&rh);
                }
                to_remove.retain(|rh| !ready.contains(rh));
            }

            dispatch_udp_packets(inner);
        });
        // poll 结束后同步所有 TCP socket 的 IO 事件到 pollee（对标 DragonOS on_iface_events）
        {
            let sockets = crate::net::TCP_SOCKETS.lock();
            for weak in sockets.iter() {
                if let Some(socket) = weak.upgrade() {
                    socket.update_io_events();
                }
            }
        }
        // poll 结束后唤醒所有 TCP/RAW socket 的等待队列
        crate::net::wake_tcp_waiters();
        crate::net::wake_raw_waiters();
    }
    pub fn remove(&self, handler: SocketHandle) {
        self._remove(handler)
    }
    pub fn _remove(&self, handler: SocketHandle) {
        if let Some(inner) = self.inner.lock().as_mut() {
            inner.sockets.remove(handler);
        }
    }

    pub fn add_routed_socket<T>(&self, proto: InetProtocol, socket: T) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let handle = inner_ref.sockets.add(socket);
        let id = inner_ref.next_socket_id;
        inner_ref.next_socket_id += 1;
        let route_handle = RouteSocketHandle(id);
        inner_ref.bindings.insert(
            route_handle,
            SocketBinding {
                ifindex: 0, // single-stack placeholder
                handle,
                proto,
            },
        );
        Some(route_handle)
    }

    pub fn tcp_routed_socket<T>(
        &self,
        rh: RouteSocketHandle,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let binding = *inner_ref.bindings.get(&rh)?;
        let socket = inner_ref.sockets.get_mut::<tcp::Socket>(binding.handle);
        Some(f(socket))
    }

    pub fn udp_routed_socket<T>(
        &self,
        rh: RouteSocketHandle,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let binding = *inner_ref.bindings.get(&rh)?;
        let socket = inner_ref.sockets.get_mut::<udp::Socket>(binding.handle);
        Some(f(socket))
    }

    pub fn tcp_connect(
        &self,
        rh: RouteSocketHandle,
        remote: smoltcp::wire::IpEndpoint,
        local: smoltcp::wire::IpEndpoint,
    ) -> Option<Result<(), smoltcp::socket::tcp::ConnectError>> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let binding = *inner_ref.bindings.get(&rh)?;
        let socket = inner_ref.sockets.get_mut::<tcp::Socket>(binding.handle);
        Some(socket.connect(inner_ref.iface.context(), remote, local))
    }

    pub fn remove_routed(&self, rh: RouteSocketHandle) {
        let mut inner = self.inner.lock();
        if let Some(inner_ref) = inner.as_mut() {
            if let Some(binding) = inner_ref.bindings.remove(&rh) {
                inner_ref.sockets.remove(binding.handle);
            }
        }
    }

    pub fn raw_routed_socket<T>(
        &self,
        rh: RouteSocketHandle,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let binding = *inner_ref.bindings.get(&rh)?;
        let socket = inner_ref.sockets.get_mut::<raw::Socket>(binding.handle);
        Some(f(socket))
    }
}

pub fn lookup_source_ip(dest_ip: IpAddress) -> IpAddress {
    let result = crate::net::routing::route_output(dest_ip)
        .map(|r| r.source)
        .unwrap_or(IpAddress::v4(0, 0, 0, 0));
    log::debug!("source_ip_select: dst={:?} -> src={:?}", dest_ip, result);
    result
}

/// Check whether a route exists for the given destination IP.
/// Returns Ok(()) if reachable, Err(ENETUNREACH) if no route available.
pub fn route_check(dest: IpAddress) -> Result<(), crate::utils::error::SyscallErr> {
    crate::net::routing::route_output(dest).map(|_| ())
}
