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
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "net_perf_diag")]
use core::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium, TxToken},
    socket::{dhcpv4, raw, tcp, udp, AnySocket},
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr},
};

/// 全局网络接口单例，管理所有 `DeviceStack`、socket 绑定表和 smoltcp 轮询。
///
/// # Locking
///
/// 内部使用 `Mutex<Option<NetInterfaceInner>>` 保护。`init()` 完成后 `inner` 始终为
/// `Some(…)`。硬件 IRQ 仅设置独立 pending 位，调度器在任务上下文轮询并提交
/// 需要其他子系统锁的 DHCP 租约。
///
/// # Ownership
///
/// `DeviceStack` 仅存储在 `NetInterfaceInner::stacks` 中。`add_veth_stack()`
/// / `remove_veth_stack()` 管理 veth 设备的全局注册，调用者负责传入正确的 `Arc<dyn Iface>`。
pub static NET_INTERFACE: NetInterface = NetInterface::new();
static NET_RX_INTERRUPT_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "net_perf_diag")]
const NET_PERF_REPORT_INTERVAL_SECS: usize = 2;
#[cfg(feature = "net_perf_diag")]
const NET_PERF_TIME_CHECK_MASK: usize = 0xff;
#[cfg(feature = "net_perf_diag")]
static NET_PERF_POLL_SAMPLES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_LAST_REPORT: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_FULL_POLLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_FULL_PROGRESS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_STACK_POLLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_STACK_PROGRESS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_LOCK_BUSY: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_POLL_TICKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static NET_PERF_POLL_TICKS_MAX: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "net_perf_diag")]
fn record_poll_perf(stack_only: bool, progressed: bool, lock_busy: bool, elapsed_ticks: usize) {
    if stack_only {
        NET_PERF_STACK_POLLS.fetch_add(1, AtomicOrdering::Relaxed);
        if progressed {
            NET_PERF_STACK_PROGRESS.fetch_add(1, AtomicOrdering::Relaxed);
        }
    } else {
        NET_PERF_FULL_POLLS.fetch_add(1, AtomicOrdering::Relaxed);
        if progressed {
            NET_PERF_FULL_PROGRESS.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }
    if lock_busy {
        NET_PERF_LOCK_BUSY.fetch_add(1, AtomicOrdering::Relaxed);
    }
    NET_PERF_POLL_TICKS.fetch_add(elapsed_ticks, AtomicOrdering::Relaxed);
    NET_PERF_POLL_TICKS_MAX.fetch_max(elapsed_ticks, AtomicOrdering::Relaxed);

    let samples = NET_PERF_POLL_SAMPLES.fetch_add(1, AtomicOrdering::Relaxed) + 1;
    if samples & NET_PERF_TIME_CHECK_MASK != 0 {
        return;
    }

    let now = crate::hal::get_time();
    let frequency = crate::hal::get_clock_freq().max(1);
    let previous = NET_PERF_LAST_REPORT.load(AtomicOrdering::Relaxed);
    if previous == 0 {
        let _ = NET_PERF_LAST_REPORT.compare_exchange(
            0,
            now,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        );
        return;
    }
    let elapsed_ticks = now.wrapping_sub(previous);
    if elapsed_ticks < frequency.saturating_mul(NET_PERF_REPORT_INTERVAL_SECS) {
        return;
    }
    if NET_PERF_LAST_REPORT
        .compare_exchange(
            previous,
            now,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        )
        .is_err()
    {
        return;
    }

    let elapsed_ms = elapsed_ticks.saturating_mul(1000) / frequency;
    let full_polls = NET_PERF_FULL_POLLS.swap(0, AtomicOrdering::Relaxed);
    let full_progress = NET_PERF_FULL_PROGRESS.swap(0, AtomicOrdering::Relaxed);
    let stack_polls = NET_PERF_STACK_POLLS.swap(0, AtomicOrdering::Relaxed);
    let stack_progress = NET_PERF_STACK_PROGRESS.swap(0, AtomicOrdering::Relaxed);
    let lock_busy = NET_PERF_LOCK_BUSY.swap(0, AtomicOrdering::Relaxed);
    let poll_ticks = NET_PERF_POLL_TICKS.swap(0, AtomicOrdering::Relaxed);
    let poll_ticks_max = NET_PERF_POLL_TICKS_MAX.swap(0, AtomicOrdering::Relaxed);
    let poll_permille = poll_ticks.saturating_mul(1000) / elapsed_ticks.max(1);
    let poll_count = full_polls.saturating_add(stack_polls);
    let poll_ticks_avg = poll_ticks / poll_count.max(1);
    println!(
        "[net-perf][poll] dt_ms={} full={}/{} stack={}/{} lock_busy={} cpu_permille={} ticks_avg={} ticks_max={}",
        elapsed_ms,
        full_progress,
        full_polls,
        stack_progress,
        stack_polls,
        lock_busy,
        poll_permille,
        poll_ticks_avg,
        poll_ticks_max
    );
}

/// 初始化网络子系统。必须先调用 `net_core::init()` 注册 lo/eth0 设备，
/// 再调用本函数创建对应的 `smoltcp::Interface` 并启动 DHCP 探测。
///
/// 如果 `NET_DEVICE` 中无网卡，仅启用 loopback。
pub fn init() {
    // Initialize net_core first (registers lo and eth0 into the netns device list).
    // Must happen before NET_INTERFACE.init() so that NetInterfaceInner::new()
    // can read IP addresses from the netns device list.
    let has_nic = NET_DEVICE.lock().is_some();
    net_core::init();
    NET_INTERFACE.init();
    #[cfg(feature = "net_perf_diag")]
    println!(
        "[net-perf] tcp_buffer rx={} tx={} listen={} bytes",
        crate::net::socket::inet::stream::inner::DEFAULT_RX_BUF_SIZE,
        crate::net::socket::inet::stream::inner::DEFAULT_TX_BUF_SIZE,
        crate::net::socket::inet::stream::inner::LISTEN_BUFFER_SIZE
    );
    if has_nic {
        boot_trace!("[kernel] net interface initialized (RoutingDevice: lo + eth)");
    } else {
        boot_trace!("[kernel] net interface initialized (loopback only, no NIC)");
    }
}

/// smoltcp 网络栈包装器，通过 `DeviceStack` 将每个网卡与一个 `smoltcp::Interface`
/// 和一个 `SocketSet` 关联。
///
/// # Locking
///
/// 所有公开轮询方法获取 `self.inner` 锁。IRQ 回调只发布 pending 位，
/// `poll_once()` 在持锁期间遍历所有 stack，不能从持锁路径中重入。
///
/// # Ownership
///
/// 全局单例 `NET_INTERFACE` 拥有所有 `DeviceStack`。socket 通过 `RouteSocketHandle`
/// 在 `bindings` 表中索引到具体的 `SocketHandle`。
pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}

/// 一个网卡设备对应的完整 smoltcp 栈上下文。
///
/// 包含设备适配器（`IfaceDevice`）、`smoltcp::Interface` 和附带的 `SocketSet`，
/// 以及对应的 `net_core` 设备元数据（`nic: Arc<dyn Iface>`）。
///
/// `sockets` 中的 `SocketHandle` 通过 `NetInterfaceInner::bindings` 表映射到
/// 内核级 `RouteSocketHandle`。
pub struct DeviceStack<'a> {
    /// 来自 `net_core` 的设备元数据（名称、ifindex、flags 等）。
    pub nic: Arc<dyn Iface>,
    pub device: IfaceDevice,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
    /// Persistent DHCP socket for interfaces that require lease renewal.
    pub dhcp_handle: Option<SocketHandle>,
    /// Latest lease event awaiting commit outside interrupt context.
    pending_dhcp_event: Option<DhcpLeaseEvent>,
}

enum DhcpLeaseEvent {
    Configured {
        address: smoltcp::wire::Ipv4Cidr,
        router: Option<smoltcp::wire::Ipv4Address>,
        dns_servers: Vec<smoltcp::wire::Ipv4Address>,
    },
    Deconfigured,
}

fn take_dhcp_event(stack: &mut DeviceStack<'_>) -> Option<DhcpLeaseEvent> {
    let handle = stack.dhcp_handle?;
    let event = match stack.sockets.get_mut::<dhcpv4::Socket>(handle).poll()? {
        dhcpv4::Event::Configured(config) => DhcpLeaseEvent::Configured {
            address: config.address,
            router: config.router,
            dns_servers: config.dns_servers.iter().copied().collect(),
        },
        dhcpv4::Event::Deconfigured => DhcpLeaseEvent::Deconfigured,
    };

    match &event {
        DhcpLeaseEvent::Configured {
            address, router, ..
        } => {
            stack.iface.update_ip_addrs(|addrs| {
                addrs.retain(|cidr| !matches!(cidr, IpCidr::Ipv4(_)));
                let _ = addrs.push(IpCidr::Ipv4(*address));
            });
            stack.iface.routes_mut().remove_default_ipv4_route();
            if let Some(router) = router {
                if stack
                    .iface
                    .routes_mut()
                    .add_default_ipv4_route(*router)
                    .is_err()
                {
                    log::error!("[net::dhcp] smoltcp route table is full");
                }
            }
        }
        DhcpLeaseEvent::Deconfigured => {
            stack.iface.update_ip_addrs(|addrs| {
                addrs.retain(|cidr| !matches!(cidr, IpCidr::Ipv4(_)));
            });
            stack.iface.routes_mut().remove_default_ipv4_route();
        }
    }
    Some(event)
}

fn capture_dhcp_event(stack: &mut DeviceStack<'_>) -> bool {
    match take_dhcp_event(stack) {
        Some(event) => {
            // Only the newest state matters: Configured followed by
            // Deconfigured must not briefly publish the stale lease.
            stack.pending_dhcp_event = Some(event);
            true
        }
        None => false,
    }
}

fn commit_dhcp_event(ifindex: u32, event: DhcpLeaseEvent) {
    match event {
        DhcpLeaseEvent::Configured {
            address,
            router,
            dns_servers,
        } => {
            let cidr = IpCidr::Ipv4(address);
            net_core::set_eth0_ipv4(cidr);
            net_core::set_default_gateway(router);
            net_core::set_dns_servers(&dns_servers);
            net_core::current_netns()
                .router
                .lock()
                .replace_dhcp_ipv4(ifindex, Some(cidr), router);
            println!(
                "[net] DHCP configured eth0={:?} gateway={:?} dns={:?}",
                address, router, dns_servers
            );
        }
        DhcpLeaseEvent::Deconfigured => {
            net_core::clear_eth0_ipv4();
            net_core::set_default_gateway(None);
            net_core::set_dns_servers(&[]);
            net_core::current_netns()
                .router
                .lock()
                .replace_dhcp_ipv4(ifindex, None, None);
            println!("[net] DHCP lease lost on eth0; discovery restarted");
        }
    }
}

/// `NetInterfaceInner` 持有所有 `DeviceStack`、socket 路由绑定表和 socket ID 计数器。
///
/// # Fields
///
/// - `stacks`: 每个已注册网卡一个 `DeviceStack`（顺序固定：lo=0, eth=1, veth…）
/// - `bindings`: 将内核级 `RouteSocketHandle` 映射到 smoltcp `SocketHandle` + ifindex
/// - `next_socket_id`: 单调递增的 socket ID 分配器
pub struct NetInterfaceInner<'a> {
    pub stacks: Vec<DeviceStack<'a>>,
    pub bindings: BTreeMap<RouteSocketHandle, SocketBinding>,
    pub next_socket_id: usize,
}

/// 延迟发送队列（全局，独立于 `NET_INTERFACE.inner` 锁）。
///
/// `NetTxToken::consume` 在 smoltcp poll 的 `inner_handler` 内执行（已持有
/// `NET_INTERFACE.inner` 锁），此时中断关闭、VirtIO 发送会忙等 completion
/// 而永远等不到（单核 SIE=0），导致内核死锁。此类数据包放入本队列，由
/// 下次调度器上下文（中断开启）的 `poll_once` 在真正持有 device 访问权时
/// 取出发送。队列独立锁避免与 `inner` 锁形成重入死锁。
static DEFERRED_TX_QUEUE: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

/// 延迟发送队列的最大积压包数。超过后丢弃最老包并计数，
/// 防止 syscall 上下文持续 egress 导致无限增长。
const DEFERRED_TX_MAX_PACKETS: usize = 64;

impl<'a> NetInterfaceInner<'a> {
    pub(crate) fn stack_mut(&mut self, ifindex: u32) -> Option<&mut DeviceStack<'a>> {
        self.stacks
            .iter_mut()
            .find(|s| s.nic.nic_id() as u32 == ifindex)
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
                    crate::net::net_core::IFF_UP
                        | crate::net::net_core::IFF_LOOPBACK
                        | crate::net::net_core::IFF_RUNNING,
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
                addrs
                    .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
                    .unwrap();
                addrs
                    .push(IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128))
                    .unwrap();
            });
            stacks.push(DeviceStack {
                nic: lo_nic,
                device: lo_device,
                iface: lo_iface,
                sockets: lo_sockets,
                dhcp_handle: None,
                pending_dhcp_event: None,
            });
        }

        // Stack 1: ethernet (ifindex=2)
        let (eth_adapter, hw_addr, has_real_nic) = match NET_DEVICE.lock().take() {
            Some(net_device) => {
                let mac = net_device.mac_address();
                (
                    SmoltcpDeviceAdapter::new(net_device),
                    EthernetAddress(mac),
                    true,
                )
            }
            None => {
                boot_trace!("[kernel] No net device, using null device (loopback only)");
                let null_dev = Arc::new(NullNetDevice);
                let null_mac = [0x02u8, 0, 0, 0, 0, 1];
                (
                    SmoltcpDeviceAdapter::new(null_dev),
                    EthernetAddress(null_mac),
                    false,
                )
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
            let mut runtime_dhcp_handle = None;

            #[cfg(all(
                feature = "boot_la_uboot_dmw",
                feature = "gmac_2k1000",
                not(feature = "gmac_dhcp")
            ))]
            if has_real_nic {
                let static_cidr = IpCidr::new(IpAddress::v4(192, 168, 9, 20), 24);
                net_core::set_eth0_ipv4(static_cidr);
                net_core::set_default_gateway(None);
                println!("[net] eth0 static address 192.168.9.20/24");
            }

            #[cfg(all(feature = "boot_la_uboot_dmw", feature = "gmac_dhcp"))]
            if has_real_nic {
                let mut dhcp_socket = dhcpv4::Socket::new();
                dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
                    discover_timeout: Duration::from_secs(2),
                    initial_request_timeout: Duration::from_secs(1),
                    request_retries: 3,
                    min_renew_timeout: Duration::from_secs(60),
                    ..dhcpv4::RetryConfig::default()
                });
                runtime_dhcp_handle = Some(eth_sockets.add(dhcp_socket));
                println!("[net] eth0 DHCP client started");
            }

            #[cfg(not(all(feature = "boot_la_uboot_dmw", feature = "gmac_2k1000")))]
            if has_real_nic {
                // Defer DHCP probing until the scheduler is running. Polling a
                // virtio device during early boot can wait indefinitely before
                // PID1 has a chance to start.
                let mut dhcp_socket = dhcpv4::Socket::new();
                dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
                    discover_timeout: Duration::from_secs(2),
                    initial_request_timeout: Duration::from_secs(1),
                    request_retries: 3,
                    min_renew_timeout: Duration::from_secs(60),
                    ..dhcpv4::RetryConfig::default()
                });
                runtime_dhcp_handle = Some(eth_sockets.add(dhcp_socket));
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
                dhcp_handle: runtime_dhcp_handle,
                pending_dhcp_event: None,
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
    /// Publish receive work from a hardware IRQ without entering smoltcp.
    #[inline(always)]
    pub fn notify_rx_interrupt(&self) {
        NET_RX_INTERRUPT_PENDING.store(true, Ordering::Release);
    }

    /// Consume the receive-work notification in scheduler task context.
    #[inline(always)]
    pub fn take_rx_interrupt(&self) -> bool {
        NET_RX_INTERRUPT_PENDING.swap(false, Ordering::AcqRel)
    }

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
        Some(
            self.inner
                .lock()
                .as_mut()?
                .stack_mut(ifindex)?
                .sockets
                .add(socket),
        )
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
                dhcp_handle: None,
                pending_dhcp_event: None,
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

    /// Return the ifindex of every currently-registered DeviceStack.
    pub fn stack_ifindexes(&self) -> Vec<u32> {
        self.inner_handler(|inner| inner.stacks.iter().map(|s| s.nic.nic_id() as u32).collect())
            .unwrap_or_default()
    }

    /// 返回 (tcp_count, udp_count, raw_count, pending_remove)
    pub fn socket_stats(&self) -> (usize, usize, usize, usize) {
        let tcp = crate::net::TCP_SOCKETS.lock().len();
        let raw = crate::net::RAW_SOCKETS.lock().len();
        let pending = TCP_SOCKETS_TO_REMOVE.lock().len() + UDP_SOCKETS_TO_REMOVE.lock().len();
        // UDP: count via inner sockets (only if initialized)
        let udp = match self.inner.lock().as_ref() {
            Some(inner) => {
                let tcp_count = inner
                    .stacks
                    .iter()
                    .flat_map(|s| s.sockets.iter())
                    .filter(|(_h, sock)| matches!(sock, smoltcp::socket::Socket::Tcp(_)))
                    .count();
                let raw_count = inner
                    .stacks
                    .iter()
                    .flat_map(|s| s.sockets.iter())
                    .filter(|(_h, sock)| matches!(sock, smoltcp::socket::Socket::Raw(_)))
                    .count();
                inner
                    .stacks
                    .iter()
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
            crate::task::perf::record_net_poll(false, false);
            #[cfg(feature = "net_perf_diag")]
            record_poll_perf(false, false, false, 0);
            return;
        }
        #[cfg(feature = "net_perf_diag")]
        let poll_start = crate::hal::get_time();
        let progressed = self.poll_once(true);
        crate::task::perf::record_net_poll(progressed, false);
        #[cfg(feature = "net_perf_diag")]
        record_poll_perf(
            false,
            progressed,
            false,
            crate::hal::get_time().wrapping_sub(poll_start),
        );
    }

    /// 将数据包加入延迟发送队列（在中断关闭的 syscall 上下文中调用）。
    ///
    /// 队列有界（`DEFERRED_TX_MAX_PACKETS`）；超限时丢弃最老包并计数，
    /// 防止 syscall 上下文持续 egress 时无界增长。
    pub(crate) fn push_deferred_tx(&self, packet: Vec<u8>) {
        let mut queue = DEFERRED_TX_QUEUE.lock();
        if queue.len() >= DEFERRED_TX_MAX_PACKETS {
            queue.remove(0);
            crate::task::perf::record_net_tx_deferred_dropped();
        }
        queue.push(packet);
    }

    /// Non-blocking task-context poll: skip if the inner lock is already held.
    /// Lease events are committed after the interface lock is released.
    pub fn try_poll(&self) -> bool {
        let guard = self.inner.try_lock();
        match guard {
            Some(inner) if inner.is_some() => {
                drop(inner);
                #[cfg(feature = "net_perf_diag")]
                let poll_start = crate::hal::get_time();
                let progressed = self.poll_once(true);
                crate::task::perf::record_net_poll(progressed, false);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(
                    false,
                    progressed,
                    false,
                    crate::hal::get_time().wrapping_sub(poll_start),
                );
                true
            }
            Some(_) => {
                crate::task::perf::record_net_poll(false, false);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(false, false, false, 0);
                false
            }
            None => {
                crate::task::perf::record_net_poll(false, true);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(false, false, true, 0);
                false
            }
        }
    }

    /// Non-blocking poll ONLY the specified stack (by ifindex).
    /// Skips remove-list draining and accept scanning — those are handled by
    /// the periodic full poll in the idle loop.
    pub fn try_poll_stack(&self, ifindex: u32) -> bool {
        let mut guard = match self.inner.try_lock() {
            Some(g) => g,
            None => {
                crate::task::perf::record_net_poll(false, true);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(true, false, true, 0);
                return false;
            }
        };
        let inner = match guard.as_mut() {
            Some(i) => i,
            None => {
                crate::task::perf::record_net_poll(false, false);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(true, false, false, 0);
                return false;
            }
        };
        let stack = match inner.stack_mut(ifindex) {
            Some(s) => s,
            None => {
                crate::task::perf::record_net_poll(false, false);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(true, false, false, 0);
                return false;
            }
        };

        use crate::net::neighbour::CURRENT_POLL_IFINDEX;
        use crate::net::socket::inet::datagram::udp::dispatch_udp_packets;
        use smoltcp::time::Instant;

        *CURRENT_POLL_IFINDEX.lock() = stack.nic.nic_id() as u32;

        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        #[cfg(feature = "net_perf_diag")]
        let poll_start = crate::hal::get_time();
        let mut progressed = stack.iface.poll(now, &mut stack.device, &mut stack.sockets);
        progressed |= capture_dhcp_event(stack);
        let dhcp_event = stack
            .pending_dhcp_event
            .take()
            .map(|event| (ifindex, event));
        dispatch_udp_packets(&mut stack.sockets);
        drop(guard);

        if let Some((ifindex, event)) = dhcp_event {
            commit_dhcp_event(ifindex, event);
        }

        if progressed {
            crate::net::wake_tcp_waiters();
            crate::net::wake_udp_waiters();
            crate::net::wake_raw_waiters();
        }
        crate::task::perf::record_net_poll(progressed, false);
        crate::net::wake_tcp_accept_waiters();
        #[cfg(feature = "net_perf_diag")]
        record_poll_perf(
            true,
            progressed,
            false,
            crate::hal::get_time().wrapping_sub(poll_start),
        );
        progressed
    }

    fn poll_once(&self, commit_dhcp: bool) -> bool {
        let mut progressed = false;
        let mut dhcp_events = Vec::new();
        self.inner_handler(|inner| {
            // Pre-collect all removal handles with their ifindex
            let udp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
                to_remove
                    .drain(..)
                    .map(|rh| {
                        let ifindex = inner
                            .bindings
                            .get(&rh)
                            .map(|b| b.ifindex)
                            .or_else(|| {
                                crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex)
                            })
                            .unwrap_or(1);
                        (inner.resolve(rh), ifindex, rh)
                    })
                    .collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove
                    .drain(..)
                    .map(|rh| {
                        let ifindex = inner
                            .bindings
                            .get(&rh)
                            .map(|b| b.ifindex)
                            .or_else(|| {
                                crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex)
                            })
                            .unwrap_or(1);
                        (inner.resolve(rh), ifindex, rh)
                    })
                    .collect()
            };

            for stack in inner.stacks.iter_mut() {
                // Set the current poll ifindex so ARP interceptors
                // can tag neighbour entries with the correct interface.
                *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock() = stack.nic.nic_id() as u32;

                // 0. Drain packets deferred from interrupt-disabled syscall
                // contexts. We are now in scheduler/task context with
                // interrupts enabled, so the blocking VirtIO transmit can
                // wait for (and receive) completion interrupts.
                if !DEFERRED_TX_QUEUE.lock().is_empty() {
                    let drain_now = Instant::from_millis(current_time_duration().as_millis() as i64);
                    let packets = core::mem::take(&mut *DEFERRED_TX_QUEUE.lock());
                    for packet in packets {
                        if let Some(token) = stack.device.transmit(drain_now) {
                            token.consume(packet.len(), |buf| {
                                buf.copy_from_slice(&packet);
                            });
                        } else {
                            // Device not ready; keep the packet for a later poll.
                            DEFERRED_TX_QUEUE.lock().push(packet);
                        }
                    }
                    progressed = true;
                }

                // 1. Clean up UDP sockets belonging to this stack
                for (resolved, ifindex, rh) in &udp_removes {
                    if *ifindex as usize == stack.nic.nic_id() {
                        if let Some(h) = resolved {
                            stack.sockets.remove(*h);
                        }
                        inner.bindings.remove(rh);
                    }
                }

                // 1.5. Deliver raw frames to packet sockets before smoltcp consumes them
                {
                    let nic_id = stack.nic.nic_id() as u32;
                    if let IfaceDevice::Veth(ref veth_driver) = stack.device {
                        let rx_queue = veth_driver.inner.rx_queue.lock();
                        crate::net::socket::packet::deliver_frames_from_veth_queue(
                            nic_id, &rx_queue,
                        );
                    }
                }

                // 2. Drive protocol stack
                let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
                progressed |= stack
                    .iface
                    .poll(timestamp, &mut stack.device, &mut stack.sockets);
                if capture_dhcp_event(stack) {
                    progressed = true;
                }
                if commit_dhcp {
                    if let Some(event) = stack.pending_dhcp_event.take() {
                        dhcp_events.push((stack.nic.nic_id() as u32, event));
                    }
                }

                // 3. Clean up TCP sockets belonging to this stack
                for (resolved, ifindex, rh) in &tcp_removes {
                    if *ifindex as usize != stack.nic.nic_id() {
                        continue;
                    }
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
        for (ifindex, event) in dhcp_events {
            commit_dhcp_event(ifindex, event);
        }
        // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        if progressed {
            crate::net::wake_tcp_waiters();
            crate::net::wake_raw_waiters();
        }

        // Unconditional listener accept scan — catches new connections
        // even when smoltcp didn't report poll progress.
        crate::net::wake_tcp_accept_waiters();

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
                to_remove
                    .drain(..)
                    .map(|rh| {
                        let ifindex = inner
                            .bindings
                            .get(&rh)
                            .map(|b| b.ifindex)
                            .or_else(|| {
                                crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex)
                            })
                            .unwrap_or(1);
                        (inner.resolve(rh), ifindex, rh)
                    })
                    .collect()
            };
            let tcp_removes: Vec<(Option<SocketHandle>, u32, RouteSocketHandle)> = {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                to_remove
                    .drain(..)
                    .map(|rh| {
                        let ifindex = inner
                            .bindings
                            .get(&rh)
                            .map(|b| b.ifindex)
                            .or_else(|| {
                                crate::net::net_core::find_by_name("eth0").map(|d| d.ifindex)
                            })
                            .unwrap_or(1);
                        (inner.resolve(rh), ifindex, rh)
                    })
                    .collect()
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

                *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock() = stack.nic.nic_id() as u32;

                // Deliver raw frames to packet sockets before smoltcp consumes them
                {
                    let nic_id = stack.nic.nic_id() as u32;
                    if let IfaceDevice::Veth(ref veth_driver) = stack.device {
                        let rx_queue = veth_driver.inner.rx_queue.lock();
                        crate::net::socket::packet::deliver_frames_from_veth_queue(
                            nic_id, &rx_queue,
                        );
                    }
                }

                stack.iface.poll(
                    Instant::from_millis(current_time_duration().as_millis() as i64),
                    &mut stack.device,
                    &mut stack.sockets,
                );

                for (resolved, ifindex, rh) in &tcp_removes {
                    if *ifindex as usize != stack.nic.nic_id() {
                        continue;
                    }
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
        let target_ifindex = net_core::default_iface().map(|d| d.ifindex).unwrap_or(1);
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
