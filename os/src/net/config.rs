use super::Mutex;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{IfaceDevice, NullNetDevice, SmoltcpDeviceAdapter};
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

pub struct DeviceStack<'a> {
    pub ifindex: u32,
    pub name: &'static str,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}

pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}

impl<'a> NetInterfaceInner<'a> {
    fn stack_mut(&mut self, ifindex: u32) -> Option<&mut DeviceStack<'a>> {
        self.stacks.iter_mut().find(|s| s.ifindex == ifindex)
    }

    fn resolve(&self, rh: RouteSocketHandle) -> Option<SocketHandle> {
        self.bindings.get(&rh).map(|b| b.handle)
    }

    fn new() -> Self {
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mut stacks = Vec::new();

        // Stack 0: loopback (ifindex=1)
        {
            let mut lo_device = IfaceDevice::Lo(Loopback::new(Medium::Ip));
            let lo_config = Config::new(HardwareAddress::Ip);
            let mut lo_iface = Interface::new(lo_config, &mut lo_device, now);
            let mut lo_sockets = SocketSet::new(vec![]);
            lo_iface.update_ip_addrs(|addrs| {
                addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)).unwrap();
            });
            stacks.push(DeviceStack {
                ifindex: 1,
                name: "lo",
                device: lo_device,
                iface: lo_iface,
                sockets: lo_sockets,
            });
        }

        // Stack 1: ethernet (ifindex=2)
        let (eth_adapter, hw_addr, has_real_nic) = match NET_DEVICE.lock().take() {
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

        {
            let mut eth_device = IfaceDevice::Eth(eth_adapter);
            let eth_config = Config::new(HardwareAddress::Ethernet(hw_addr));
            let mut eth_iface = Interface::new(eth_config, &mut eth_device, now);
            let mut eth_sockets = SocketSet::new(vec![]);

            if has_real_nic {
                // DHCP probe
                let mut dhcp_socket = dhcpv4::Socket::new();
                dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
                    discover_timeout: Duration::from_secs(2),
                    initial_request_timeout: Duration::from_secs(1),
                    request_retries: 3,
                    min_renew_timeout: Duration::from_secs(60),
                    ..dhcpv4::RetryConfig::default()
                });
                let dhcp_handle = eth_sockets.add(dhcp_socket);
                let deadline = Instant::from_millis(
                    current_time_duration().as_millis() as i64 + 5000,
                );

                loop {
                    let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
                    eth_iface.poll(timestamp, &mut eth_device, &mut eth_sockets);

                    let event = eth_sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll();
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
                eth_sockets.remove(dhcp_handle);
            }

            // Source IP from net_core (DHCP result)
            let addrs_src: Vec<IpCidr> = {
                let ifaces = net_core::IFACES.lock();
                ifaces.iter().filter(|d| d.ifindex == 2)
                    .flat_map(|dev| dev.ip_addrs.iter().copied())
                    .collect()
            };
            if !addrs_src.is_empty() {
                eth_iface.update_ip_addrs(|addrs| {
                    for cidr in &addrs_src {
                        addrs.push(*cidr).unwrap();
                    }
                });
            }
            log::info!("[net::config] eth0 addresses: {:?}", addrs_src);

            if let Some(gw) = net_core::default_gateway() {
                eth_iface.routes_mut().add_default_ipv4_route(gw).unwrap();
            }

            stacks.push(DeviceStack {
                ifindex: 2,
                name: "eth0",
                device: eth_device,
                iface: eth_iface,
                sockets: eth_sockets,
            });
        }

        log::info!("[net::config] initialized {} stacks", stacks.len());
        Self {
            stacks,
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
        Some(self.inner.lock().as_mut()?.stacks[0].sockets.add(socket))
    }

    pub fn tcp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.stacks[0].sockets.get_mut::<tcp::Socket>(handler);
        Some(f(socket))
    }

    pub fn udp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.stacks[0].sockets.get_mut::<udp::Socket>(handler);
        Some(f(socket))
    }

    pub fn raw_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.stacks[0].sockets.get_mut::<raw::Socket>(handler);
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
            Some(inner) => inner.stacks[0].sockets.iter().count().saturating_sub(tcp).saturating_sub(raw),
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
            // Pre-collect all removal handles with their ifindex
            let udp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex).unwrap_or(2);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex).unwrap_or(2);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };

            for stack in inner.stacks.iter_mut() {
                // 1. Clean up UDP sockets belonging to this stack
                for (resolved, ifindex, rh) in &udp_removes {
                    if *ifindex == stack.ifindex {
                        if let Some(h) = resolved {
                            stack.sockets.remove(*h);
                        }
                        inner.bindings.remove(rh);
                    }
                }

                // 2. Drive protocol stack
                let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
                progressed |= stack
                    .iface
                    .poll(timestamp, &mut stack.device, &mut stack.sockets);

                // 3. Clean up TCP sockets belonging to this stack
                for (resolved, ifindex, rh) in &tcp_removes {
                    if *ifindex != stack.ifindex { continue; }
                    let can_remove = match resolved {
                        Some(h) => {
                            let socket = stack.sockets.get::<tcp::Socket>(*h);
                            socket.state() == tcp::State::Closed
                        }
                        None => true,
                    };
                    if can_remove {
                        if let Some(h) = resolved {
                            stack.sockets.remove(*h);
                        }
                        inner.bindings.remove(rh);
                    } else {
                        TCP_SOCKETS_TO_REMOVE.lock().push(*rh);
                    }
                }

                // 4. Dispatch UDP packets for this stack
                dispatch_udp_packets(&mut stack.sockets);
            }
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
            let udp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex).unwrap_or(2);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex).unwrap_or(2);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };

            for stack in inner.stacks.iter_mut() {
                for (resolved, ifindex, rh) in &udp_removes {
                    if *ifindex == stack.ifindex {
                        if let Some(h) = resolved {
                            stack.sockets.remove(*h);
                        }
                        inner.bindings.remove(rh);
                    }
                }

                stack.iface.poll(
                    Instant::from_millis(current_time_duration().as_millis() as i64),
                    &mut stack.device,
                    &mut stack.sockets,
                );

                for (resolved, ifindex, rh) in &tcp_removes {
                    if *ifindex != stack.ifindex { continue; }
                    let can_remove = match resolved {
                        Some(h) => {
                            let socket = stack.sockets.get::<tcp::Socket>(*h);
                            socket.state() == tcp::State::Closed
                                || socket.state() == tcp::State::TimeWait
                        }
                        None => true,
                    };
                    if can_remove {
                        if let Some(h) = resolved {
                            stack.sockets.remove(*h);
                        }
                        inner.bindings.remove(rh);
                    } else {
                        TCP_SOCKETS_TO_REMOVE.lock().push(*rh);
                    }
                }

                dispatch_udp_packets(&mut stack.sockets);
            }
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
            inner.stacks[0].sockets.remove(handler);
        }
    }

    pub fn add_routed_socket<T>(&self, proto: InetProtocol, socket: T) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let target_ifindex = if inner_ref.stack_mut(2).is_some() { 2 } else { 1 };
        let stack = inner_ref.stack_mut(target_ifindex)?;
        let handle = stack.sockets.add(socket);
        let id = inner_ref.next_socket_id;
        inner_ref.next_socket_id += 1;
        let route_handle = RouteSocketHandle(id);
        inner_ref.bindings.insert(
            route_handle,
            SocketBinding {
                ifindex: target_ifindex,
                handle,
                proto,
            },
        );
        Some(route_handle)
    }

    pub fn add_routed_socket_on<T>(
        &self,
        proto: InetProtocol,
        socket: T,
        ifindex: u32,
    ) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let stack = inner_ref.stack_mut(ifindex)?;
        let handle = stack.sockets.add(socket);
        let id = inner_ref.next_socket_id;
        inner_ref.next_socket_id += 1;
        let route_handle = RouteSocketHandle(id);
        inner_ref.bindings.insert(
            route_handle,
            SocketBinding {
                ifindex,
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
        let stack = inner_ref.stack_mut(binding.ifindex)?;
        let socket = stack.sockets.get_mut::<tcp::Socket>(binding.handle);
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
        let stack = inner_ref.stack_mut(binding.ifindex)?;
        let socket = stack.sockets.get_mut::<udp::Socket>(binding.handle);
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
        let stack = inner_ref.stack_mut(binding.ifindex)?;
        let socket = stack.sockets.get_mut::<tcp::Socket>(binding.handle);
        Some(socket.connect(stack.iface.context(), remote, local))
    }

    pub fn remove_routed(&self, rh: RouteSocketHandle) {
        let mut inner = self.inner.lock();
        if let Some(inner_ref) = inner.as_mut() {
            let binding = inner_ref.bindings.remove(&rh);
            if let Some(b) = binding {
                if let Some(stack) = inner_ref.stack_mut(b.ifindex) {
                    stack.sockets.remove(b.handle);
                }
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
        let stack = inner_ref.stack_mut(binding.ifindex)?;
        let socket = stack.sockets.get_mut::<raw::Socket>(binding.handle);
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
