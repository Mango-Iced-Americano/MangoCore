use super::Mutex;
use crate::drivers::net::veth::VethDriver;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{IfaceDevice, NullNetDevice, SmoltcpDeviceAdapter};
use crate::net::iface::Iface;
use crate::net::net_core::{self, NetDeviceEntry};
use crate::net::routing::{InetProtocol, RouteSocketHandle, SocketBinding};
use crate::net::socket::inet::datagram::udp::dispatch_udp_packets;
use crate::net::socket::inet::stream::inner::tcp_state_code;
use crate::net::{TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS_TO_REMOVE};
use crate::timer::current_time_duration;
use crate::trace_event;
use alloc::collections::BTreeMap;
use alloc::string::String;
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
    // Initialize net_core first (registers lo and eth0 into the netns device list).
    // Must happen before NET_INTERFACE.init() so that NetInterfaceInner::new()
    // can read IP addresses from the netns device list.
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
    /// Reference to the net_core device for metadata (name, ifindex, flags, etc.).
    pub nic: Arc<dyn Iface>,
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
    pub(crate) fn stack_mut(&mut self, ifindex: u32) -> Option<&mut DeviceStack<'a>> {
        self.stacks.iter_mut().find(|s| s.nic.nic_id() as u32 == ifindex)
    }

    fn resolve(&self, rh: RouteSocketHandle) -> Option<SocketHandle> {
        self.bindings.get(&rh).map(|b| b.handle)
    }

    fn new() -> Self {
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mut stacks = Vec::new();

        // Stack 0: loopback (ifindex=1)
        let lo_nic: Arc<dyn Iface> = net_core::find_by_name("lo")
            .map(|d| d.iface)
            .unwrap_or_else(|| {
                let lo = Arc::new(NetDeviceEntry::new(
                    String::from("lo"),
                    crate::net::net_core::DeviceKind::Loopback,
                    [0u8; 6],
                    65536,
                    crate::net::net_core::IFF_UP | crate::net::net_core::IFF_LOOPBACK | crate::net::net_core::IFF_RUNNING,
                    vec![
                        IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8),
                        IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128),
                    ],
                    None,
                    crate::net::net_core::IF_OPER_UP as u32,
                ));
                lo.set_nic_id(1);
                lo
            });
        {
            let mut lo_device = IfaceDevice::Lo(Loopback::new(Medium::Ip));
            let lo_config = Config::new(HardwareAddress::Ip);
            let mut lo_iface = Interface::new(lo_config, &mut lo_device, now);
            let mut lo_sockets = SocketSet::new(vec![]);
            lo_iface.update_ip_addrs(|addrs| {
                addrs.push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)).unwrap();
                addrs.push(IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128)).unwrap();
            });
            stacks.push(DeviceStack {
                nic: lo_nic,
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

        let eth_nic: Arc<dyn Iface> = net_core::find_by_name("eth0")
            .map(|d| d.iface)
            .unwrap_or_else(|| {
                let eth = Arc::new(NetDeviceEntry::new(
                    String::from("eth0"),
                    crate::net::net_core::DeviceKind::Ethernet,
                    [0u8; 6],
                    1500,
                    crate::net::net_core::IFF_UP | crate::net::net_core::IFF_BROADCAST,
                    vec![],
                    None,
                    0,
                ));
                eth.set_nic_id(2);
                eth
            });

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
                let ns = net_core::current_netns();
                let list = ns.device_list.lock();
                list.values()
                    .filter(|iface| iface.nic_id() == 2)
                    .flat_map(|iface| iface.ip_addrs().iter().copied().collect::<Vec<_>>())
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
                nic: eth_nic,
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

    pub fn add_socket<T>(&self, ifindex: u32, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        self._add_socket(ifindex, socket)
    }

    pub fn _init(&self) {
        *self.inner.lock() = Some(NetInterfaceInner::new());
    }
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn _add_socket<T>(&self, ifindex: u32, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        Some(self.inner.lock().as_mut()?.stack_mut(ifindex)?.sockets.add(socket))
    }

    /// Add a veth device as a DeviceStack into NET_INTERFACE.
    /// Must be called after `NetInterface::init()`, otherwise the veth stack is silently dropped.
    pub fn add_veth_stack(&self, nic: Arc<dyn Iface>, device: VethDriver) {
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mac = nic.mac();
        let mut veth_device = IfaceDevice::Veth(device);
        let veth_config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        let mut veth_iface = Interface::new(veth_config, &mut veth_device, now);
        let veth_sockets = SocketSet::new(vec![]);

        let mut inner = self.inner.lock();
        if let Some(ref mut inner_ref) = *inner {
            inner_ref.stacks.push(DeviceStack {
                nic,
                device: veth_device,
                iface: veth_iface,
                sockets: veth_sockets,
            });
        }
    }

    /// Remove a veth DeviceStack identified by its nic_id.
    /// Silently returns if no matching stack exists.
    pub fn remove_veth_stack(&self, nic_id: u32) {
        let mut inner = self.inner.lock();
        if let Some(ref mut inner_ref) = *inner {
            inner_ref.stacks.retain(|s| s.nic.nic_id() as u32 != nic_id);
        }
    }

    /// Sync an IP address into the smoltcp Interface of a DeviceStack.
    pub fn add_ip_to_stack(&self, ifindex: u32, cidr: IpCidr) {
        let mut inner = self.inner.lock();
        if let Some(ref mut inner_ref) = *inner {
            if let Some(stack) = inner_ref.stack_mut(ifindex) {
                stack.iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(cidr);
                });
            }
        }
    }

    /// Remove an IP address from the smoltcp Interface of a DeviceStack.
    pub fn remove_ip_from_stack(&self, ifindex: u32, cidr: IpCidr) {
        let mut inner = self.inner.lock();
        if let Some(ref mut inner_ref) = *inner {
            if let Some(stack) = inner_ref.stack_mut(ifindex) {
                stack.iface.update_ip_addrs(|addrs| {
                    addrs.retain(|a| *a != cidr);
                });
            }
        }
    }

    pub fn tcp_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let stack = inner_ref.stack_mut(ifindex)?;
        let socket = stack.sockets.get_mut::<tcp::Socket>(handler);
        Some(f(socket))
    }

    pub fn udp_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let stack = inner_ref.stack_mut(ifindex)?;
        let socket = stack.sockets.get_mut::<udp::Socket>(handler);
        Some(f(socket))
    }

    pub fn raw_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let stack = inner_ref.stack_mut(ifindex)?;
        let socket = stack.sockets.get_mut::<raw::Socket>(handler);
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
            Some(inner) => {
                let tcp_count = inner.stacks.iter()
                    .flat_map(|s| s.sockets.iter())
                    .filter(|(_h, sock)| matches!(sock, smoltcp::socket::Socket::Tcp(_)))
                    .count();
                let raw_count = inner.stacks.iter()
                    .flat_map(|s| s.sockets.iter())
                    .filter(|(_h, sock)| matches!(sock, smoltcp::socket::Socket::Raw(_)))
                    .count();
                inner.stacks.iter()
                    .flat_map(|s| s.sockets.iter())
                    .count()
                    .saturating_sub(tcp_count)
                    .saturating_sub(raw_count)
            }
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
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex)
                        .or_else(|| crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex))
                        .unwrap_or(1);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex)
                        .or_else(|| crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex))
                        .unwrap_or(1);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };

            for stack in inner.stacks.iter_mut() {
                // 1. Clean up UDP sockets belonging to this stack
                for (resolved, ifindex, rh) in &udp_removes {
                    if *ifindex as usize == stack.nic.nic_id() {
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
                    if *ifindex as usize != stack.nic.nic_id() { continue; }
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
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex)
                        .or_else(|| crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex))
                        .unwrap_or(1);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove.drain(..).map(|rh| {
                    let ifindex = inner.bindings.get(&rh).map(|b| b.ifindex)
                        .or_else(|| crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex))
                        .unwrap_or(1);
                    (inner.resolve(rh), ifindex, rh)
                }).collect()
            };

            for stack in inner.stacks.iter_mut() {
                for (resolved, ifindex, rh) in &udp_removes {
                    if *ifindex as usize == stack.nic.nic_id() {
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
                    if *ifindex as usize != stack.nic.nic_id() { continue; }
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
    pub fn remove(&self, handler: SocketHandle, ifindex: u32) {
        self._remove(handler, ifindex)
    }
    pub fn _remove(&self, handler: SocketHandle, ifindex: u32) {
        if let Some(inner) = self.inner.lock().as_mut() {
            if let Some(stack) = inner.stack_mut(ifindex) {
                stack.sockets.remove(handler);
            }
        }
    }

    pub fn add_routed_socket<T>(&self, proto: InetProtocol, socket: T) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let target_ifindex = net_core::default_iface()
            .map(|d| d.ifindex)
            .unwrap_or(1);
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

    pub fn rebind_routed_udp(
        &self,
        rh: RouteSocketHandle,
        new_ifindex: u32,
    ) -> Option<RouteSocketHandle> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let old_binding = inner_ref.bindings.remove(&rh)?;
        if old_binding.ifindex == new_ifindex {
            inner_ref.bindings.insert(rh, old_binding);
            return Some(rh);
        }
        let rx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 1024],
            vec![0u8; crate::net::MAX_BUFFER_SIZE],
        );
        let tx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 1024],
            vec![0u8; crate::net::MAX_BUFFER_SIZE],
        );
        let new_socket = udp::Socket::new(rx_buf, tx_buf);
        {
            let old_stack = inner_ref.stack_mut(old_binding.ifindex)?;
            old_stack.sockets.remove(old_binding.handle);
        }
        let new_stack = inner_ref.stack_mut(new_ifindex)?;
        let new_handle = new_stack.sockets.add(new_socket);
        inner_ref.bindings.insert(
            rh,
            SocketBinding {
                ifindex: new_ifindex,
                handle: new_handle,
                proto: InetProtocol::Udp,
            },
        );
        Some(rh)
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
        .unwrap_or(match dest_ip {
            IpAddress::Ipv4(_) => IpAddress::v4(0, 0, 0, 0),
            IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 0),
        });
    log::debug!("source_ip_select: dst={:?} -> src={:?}", dest_ip, result);
    result
}

/// Check whether a route exists for the given destination IP.
/// Returns Ok(()) if reachable, Err(ENETUNREACH) if no route available.
pub fn route_check(dest: IpAddress) -> Result<(), crate::utils::error::SyscallErr> {
    crate::net::routing::route_output(dest).map(|_| ())
}
