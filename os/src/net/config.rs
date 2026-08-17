use super::Mutex;
use crate::drivers::net::veth::VethDriver;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{drain_deferred_tx, IfaceDevice, NullNetDevice, SmoltcpDeviceAdapter};
use crate::net::iface::Iface;
use crate::net::net_core::{self, NetDeviceEntry};
use crate::net::routing::{InetProtocol, RouteSocketHandle};
use crate::net::socket::inet::datagram::udp::{dispatch_udp_packets, drain_udp_packets};
use crate::net::socket::inet::stream::inner::tcp_state_code;
use crate::net::{
    TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS_TO_REMOVE,
};
use crate::net::socket::TcpSocketRemoval;
use crate::timer::{current_time_duration, TimeSpec};
use crate::trace_event;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
#[cfg(feature = "net_perf_diag")]
use core::sync::atomic::Ordering as AtomicOrdering;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium, TxToken},
    socket::{dhcpv4, raw, tcp, udp, AnySocket},
    time::{Duration, Instant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr},
};
use spin::Once;

/// 全局网络接口单例，管理短持 route 目录和逐设备 smoltcp 栈。
///
/// # Locking
///
/// `directory` 只保护 stack 生命周期和 route 发布；`init()` 完成后其值始终为
/// `Some(…)`。smoltcp `Interface + SocketSet` 仅由每个 `DeviceStackCell::inner` 保护。
///
/// # Ownership
///
/// `DeviceStackCell` 仅存储在 `NetDirectory::stacks` 中。`add_veth_stack()`
/// / `remove_veth_stack()` 管理 veth 设备的全局注册，调用者负责传入正确的 `Arc<dyn Iface>`。
pub static NET_INTERFACE: NetInterface = NetInterface::new();

/// smoltcp 在 graceful close 后最多保留 10 秒的 Close timer；额外留出 5 秒给
/// 一次重传/worker 调度。超时后 worker 明确 abort 并从 SocketSet 移除，不能让
/// 已无用户 owner 的大缓冲无限驻留。
const TCP_REMOVAL_GRACE_SECS: usize = 15;
static SCHEDULER_TICK_NET_FALLBACK_ENABLED: AtomicBool = AtomicBool::new(true);

/// 在短移除队列临界区内去重 UDP route。调用者必须在锁外决定是否唤醒 worker。
pub(crate) fn enqueue_udp_socket_removal(route: RouteSocketHandle) -> bool {
    let mut removals = UDP_SOCKETS_TO_REMOVE.lock();
    if removals.contains(&route) {
        return false;
    }
    removals.push(route);
    true
}

/// 发布 TCP route 的延迟回收请求。相同 route 只保留最早的强制回收 deadline；
/// listener 的 abort 要求可以覆盖普通 graceful close，避免并发 close 重复积压。
pub(crate) fn enqueue_tcp_socket_removal(
    route: RouteSocketHandle,
    abort_if_active: bool,
) -> bool {
    let deadline = TimeSpec::now() + TimeSpec::from_s(TCP_REMOVAL_GRACE_SECS);
    let mut removals = TCP_SOCKETS_TO_REMOVE.lock();
    if let Some(existing) = removals.iter_mut().find(|pending| pending.route == route) {
        existing.deadline = existing.deadline.min(deadline);
        existing.abort_if_active |= abort_if_active;
        return false;
    }
    removals.push(TcpSocketRemoval {
        route,
        deadline,
        close_started: false,
        abort_if_active,
    });
    true
}

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
/// 再调用本函数创建对应的 `smoltcp::Interface`、注册常驻 DHCP socket
/// 并发布首轮后台 poll 请求。
///
/// 如果 `NET_DEVICE` 中无网卡，仅启用 loopback。
pub fn init() {
    // Initialize net_core first (registers lo and eth0 into the netns device list).
    // Must happen before NET_INTERFACE.init() so that NetDirectory::new()
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
        boot_trace!("[kernel] net interface initialized (per-device stacks: lo + eth)");
    } else {
        boot_trace!("[kernel] net interface initialized (loopback only, no NIC)");
    }
}

/// 网络轮询 worker 的请求合并与等待域。
///
/// `pending` 是合并门：生产者只在 false -> true 时唤醒 worker；worker 先清门，
/// 再扫描，从而不会把扫描期间的新请求吞入旧轮。IRQ 只发布原子状态，WaitQueue
/// 唤醒留给安全点。
struct NetPollControl {
    pending: AtomicBool,
    deferred_wake: AtomicBool,
    /// DeviceStack try_lock 失败只置位；CPU0 下一 scheduler tick 才重新提交。
    retry_armed: AtomicBool,
    /// 每次重新计算 TCP 回收 deadline 都递增；旧的 kernel timer 到期后不会为已
    /// 回收 route 误唤醒 worker。该序号只发布 timer entry，不承担 route 所有权。
    tcp_cleanup_generation: AtomicUsize,
    /// WaitQueue 需要堆分配，因此在 worker 首次运行时构造；`pending` 自身会
    /// 保存早于初始化到达的请求，不需要为静态对象开启 `const_heap`。
    worker_wait: Once<Mutex<crate::task::WaitQueue>>,
}

impl NetPollControl {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            deferred_wake: AtomicBool::new(false),
            retry_armed: AtomicBool::new(false),
            tcp_cleanup_generation: AtomicUsize::new(0),
            worker_wait: Once::new(),
        }
    }
}

/// smoltcp 网络栈目录。
///
/// 目录只保存设备栈和 route ID 到设备栈的弱引用；绝不在目录锁内执行 smoltcp
/// 操作。这样 loopback、eth 和 veth 可以分别持有各自的 `DeviceStackCell` 锁推进。
pub struct NetInterface<'a> {
    directory: Mutex<Option<NetDirectory<'a>>>,
    /// route ID 永不复用，即使 smoltcp 的 SocketHandle slot 已被回收也不能让旧 route
    /// 指向新 socket。
    next_route_id: AtomicUsize,
    poll: NetPollControl,
}

/// 路由目录在 N0 锁域内提供短生命周期查询/发布；不能在该锁下取得 DeviceStack。
pub(crate) struct NetDirectory<'a> {
    stacks: BTreeMap<u32, Arc<DeviceStackCell<'a>>>,
    routes: BTreeMap<RouteSocketHandle, RouteDirectoryEntry<'a>>,
}

struct RouteDirectoryEntry<'a> {
    stack: Weak<DeviceStackCell<'a>>,
    protocol: InetProtocol,
    state: RouteState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteState {
    Active,
    Migrating,
    Draining,
}

const STACK_ACTIVE: u8 = 0;
const STACK_DRAINING: u8 = 1;
const STACK_DEAD: u8 = 2;

/// 每个物理/虚拟设备一个 smoltcp 串行域；同一时刻只允许持有一把该锁。
struct DeviceStackCell<'a> {
    ifindex: u32,
    state: AtomicU8,
    inner: Mutex<DeviceStackInner<'a>>,
}

struct DeviceStackInner<'a> {
    nic: Arc<dyn Iface>,
    device: IfaceDevice,
    iface: Interface,
    sockets: SocketSet<'a>,
    /// 与 SocketSet 同锁域，route 在此重验才能阻止重用 slot 的旧 route 误访问。
    bindings: BTreeMap<RouteSocketHandle, LocalSocketBinding>,
    dhcp_handle: Option<SocketHandle>,
    pending_dhcp_event: Option<DhcpLeaseEvent>,
}

#[derive(Clone, Copy)]
struct LocalSocketBinding {
    handle: SocketHandle,
    protocol: InetProtocol,
}

enum DhcpLeaseEvent {
    Configured {
        address: smoltcp::wire::Ipv4Cidr,
        router: Option<smoltcp::wire::Ipv4Address>,
        dns_servers: Vec<smoltcp::wire::Ipv4Address>,
    },
    Deconfigured,
}

fn take_dhcp_event(stack: &mut DeviceStackInner<'_>) -> Option<DhcpLeaseEvent> {
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

fn capture_dhcp_event(stack: &mut DeviceStackInner<'_>) -> bool {
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
            write_resolv_conf(&dns_servers);
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
            write_resolv_conf(&[]);
            println!("[net] DHCP lease lost on eth0; discovery restarted");
        }
    }
}

/// 把 DHCP 拿到的 DNS 服务器写入 /etc/resolv.conf（Linux dhclient 语义）。
///
/// 构建时硬编码的 resolv.conf 只适合单一环境（QEMU SLIRP 的 nameserver），
/// 换到实板 / 其它 DHCP 后会拿到错误的 DNS。DHCP 配置正确后，用户态
/// （如 inet_test 读取 resolv.conf）应使用本次租约分配的 resolver。
fn write_resolv_conf(dns_servers: &[smoltcp::wire::Ipv4Address]) {
    use crate::fs::vfs::file::{File, FileFlags};
    use crate::fs::vfs::FileType;

    // 重建完整内容（截断旧 lease 残留），保证只反映当前租约的 resolver。
    let mut content = alloc::vec::Vec::with_capacity(dns_servers.len() * 22 + 4);
    for dns in dns_servers {
        let [a, b, c, d] = dns.0;
        content.extend_from_slice(b"nameserver ");
        content.extend_from_slice(alloc::format!("{}.{}.{}.{}\n", a, b, c, d).as_bytes());
    }
    if content.is_empty() {
        content.extend_from_slice(b"# no DHCP nameserver\n");
    }

    let Ok(inode) = crate::fs::vfs_lookup_absolute("/etc/resolv.conf") else {
        // initramfs 总是提供该文件；缺失时跳过写回。
        return;
    };
    let file = File::new_without_open(inode, FileFlags::O_RDWR, FileType::File);
    let _ = file.truncate_size(0);
    let _ = file.write(&content);
}

impl<'a> NetDirectory<'a> {
    fn new() -> Self {
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mut directory = Self {
            stacks: BTreeMap::new(),
            routes: BTreeMap::new(),
        };

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
            directory.stacks.insert(
                1,
                Arc::new(DeviceStackCell {
                    ifindex: 1,
                    state: AtomicU8::new(STACK_ACTIVE),
                    inner: Mutex::new(DeviceStackInner {
                        nic: lo_nic,
                        device: lo_device,
                        iface: lo_iface,
                        sockets: lo_sockets,
                        bindings: BTreeMap::new(),
                        dhcp_handle: None,
                        pending_dhcp_event: None,
                    }),
                }),
            );
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
                // QEMU 也使用与 gmac_dhcp 相同的常驻 DHCP 上层语义。启动阶段
                // 仍处于 IRQ-off 上下文，不在这里 poll VirtIO 并轮询 TX used ring；
                // 只注册 socket，交给调度器启动后的 CPU0 worker。
                let mut dhcp_socket = dhcpv4::Socket::new();
                dhcp_socket.set_retry_config(dhcpv4::RetryConfig {
                    discover_timeout: Duration::from_secs(2),
                    initial_request_timeout: Duration::from_secs(1),
                    request_retries: 3,
                    min_renew_timeout: Duration::from_secs(60),
                    ..dhcpv4::RetryConfig::default()
                });
                runtime_dhcp_handle = Some(eth_sockets.add(dhcp_socket));
                println!("[net] eth0 DHCP client started (deferred to CPU0 poll worker)");
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

            directory.stacks.insert(
                2,
                Arc::new(DeviceStackCell {
                    ifindex: 2,
                    state: AtomicU8::new(STACK_ACTIVE),
                    inner: Mutex::new(DeviceStackInner {
                        nic: eth_nic,
                        device: eth_device,
                        iface: eth_iface,
                        sockets: eth_sockets,
                        bindings: BTreeMap::new(),
                        dhcp_handle: runtime_dhcp_handle,
                        pending_dhcp_event: None,
                    }),
                }),
            );
        }

        log::info!(
            "[net::config] initialized {} stacks",
            directory.stacks.len()
        );
        directory
    }
}

impl<'a> NetInterface<'a> {
    pub fn init(&self) {
        self._init();
        // worker 尚未创建时 request_poll() 只保留 pending=true；worker 首次进入
        // wait_event 的条件复查就会消费它，从而立即发送首个 DHCP Discover。
        self.request_poll();
    }

    pub fn add_socket<T>(&self, ifindex: u32, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        self._add_socket(ifindex, socket)
    }

    pub fn _init(&self) {
        *self.directory.lock() = Some(NetDirectory::new());
    }
    pub const fn new() -> Self {
        Self {
            directory: Mutex::new(None),
            next_route_id: AtomicUsize::new(1),
            poll: NetPollControl::new(),
        }
    }

    /// 返回 worker 唯一的等待队列。只有任务上下文会触发首次构造；请求方若在
    /// 此前到达，只需保留 `pending=true`，worker 启动后的条件检查会立即消费。
    fn worker_wait(&self) -> &Mutex<crate::task::WaitQueue> {
        self.poll
            .worker_wait
            .call_once(|| Mutex::new(crate::task::WaitQueue::new()));
        self.poll
            .worker_wait
            .get()
            .expect("net poll worker wait queue was not initialized")
    }

    /// 纯异步地请求 CPU0 poll worker 推进网络状态。
    ///
    /// `pending` 的 AcqRel test-and-set 同时发布此前的 socket 状态并充当合并门；
    /// 只有第一个未处理请求需要唤醒已经启动的 worker。
    /// 调用方不得持有 DeviceStack、socket 或 task.inner 锁。
    pub fn request_poll(&self) {
        if !self.poll.pending.swap(true, Ordering::AcqRel) {
            if let Some(wait_queue) = self.poll.worker_wait.get() {
                wait_queue.lock().wake_all();
            }
        }
    }

    /// ktest 用于确认此前的网络请求已被 worker 消费；这是纯原子观察，不触碰
    /// DeviceStack，因此可以在“关闭后没有网络流量”的 regression 中精确构造睡眠窗口。
    pub(crate) fn poll_request_pending(&self) -> bool {
        self.poll.pending.load(Ordering::Acquire)
    }

    /// 供全局 deadline queue 复核的 generation。只比较原子序号，绝不在 timer
    /// queue 锁域内取得网络目录或 DeviceStack 锁。
    pub(crate) fn tcp_cleanup_timer_is_current(&self, generation: usize) -> bool {
        self.poll.tcp_cleanup_generation.load(Ordering::Acquire) == generation
    }

    /// TCP 回收 deadline 到期后由 CPU0 timer callback 调用。队列已经被 worker
    /// 清空时这是陈旧事件；否则只发布一次 poll，不在 timer callback 触碰 smoltcp。
    pub(crate) fn run_tcp_cleanup_timer(&self, generation: usize) -> bool {
        if !self.tcp_cleanup_timer_is_current(generation) || TCP_SOCKETS_TO_REMOVE.lock().is_empty()
        {
            return false;
        }
        self.request_poll();
        true
    }

    /// 从 hard IRQ 发布一次网络推进请求。
    ///
    /// 此路径不得轮询、拿 WaitQueue、分配或输出；安全点随后把 deferred 标志转换为唤醒。
    fn kick_from_irq(&self) {
        if !self.poll.pending.swap(true, Ordering::AcqRel) {
            self.poll.deferred_wake.store(true, Ordering::Release);
        }
    }

    /// 在任务或 idle 安全点把 IRQ 的发布转换为 worker 唤醒。
    pub fn run_deferred_net_wake(&self) {
        if self.poll.deferred_wake.swap(false, Ordering::AcqRel) {
            if let Some(wait_queue) = self.poll.worker_wait.get() {
                wait_queue.lock().wake_all();
            }
        }
    }

    pub(crate) fn set_scheduler_tick_net_fallback_enabled_for_test(&self, enabled: bool) -> bool {
        SCHEDULER_TICK_NET_FALLBACK_ENABLED.swap(enabled, Ordering::AcqRel)
    }

    pub(crate) fn scheduler_tick_net_fallback_enabled(&self) -> bool {
        SCHEDULER_TICK_NET_FALLBACK_ENABLED.load(Ordering::Acquire)
    }

    pub(crate) fn run_scheduler_tick_net_fallback(&self) {
        if self.scheduler_tick_net_fallback_enabled() {
            let _ = self.try_poll_irq();
            self.run_deferred_net_wake();
        }
    }

    /// CPU0 housekeeping 消费一次忙栈 retry。不能从 worker 立即重发请求，
    /// 否则持续持有 N2 的调用者会让 worker 在内核栈上空转。
    pub fn run_deferred_poll_retry(&self) {
        if self.poll.retry_armed.swap(false, Ordering::AcqRel) {
            self.request_poll();
        }
    }

    /// 在目录锁内只克隆目标栈 Arc；调用者必须在释放目录后才取得栈锁。
    fn stack_arc(&self, ifindex: u32) -> Option<Arc<DeviceStackCell<'a>>> {
        self.directory
            .lock()
            .as_ref()?
            .stacks
            .get(&ifindex)
            .cloned()
    }

    fn active_route_stack(
        &self,
        route: RouteSocketHandle,
        protocol: InetProtocol,
    ) -> Option<Arc<DeviceStackCell<'a>>> {
        let directory = self.directory.lock();
        let entry = directory.as_ref()?.routes.get(&route)?;
        if entry.state != RouteState::Active || entry.protocol != protocol {
            return None;
        }
        entry.stack.upgrade()
    }

    pub(crate) fn routed_ifindex(&self, route: RouteSocketHandle) -> Option<u32> {
        let directory = self.directory.lock();
        let entry = directory.as_ref()?.routes.get(&route)?;
        if entry.state != RouteState::Active {
            return None;
        }
        entry.stack.upgrade().map(|stack| stack.ifindex)
    }

    pub fn _add_socket<T>(&self, ifindex: u32, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        let stack = self.stack_arc(ifindex)?;
        if stack.state.load(Ordering::Acquire) != STACK_ACTIVE {
            return None;
        }
        let mut stack_guard = stack.inner.lock();
        Some(stack_guard.sockets.add(socket))
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

        let ifindex = nic.nic_id() as u32;
        let stack = Arc::new(DeviceStackCell {
            ifindex,
            state: AtomicU8::new(STACK_ACTIVE),
            inner: Mutex::new(DeviceStackInner {
                nic,
                device: veth_device,
                iface: veth_iface,
                sockets: veth_sockets,
                bindings: BTreeMap::new(),
                dhcp_handle: None,
                pending_dhcp_event: None,
            }),
        });
        if let Some(directory) = self.directory.lock().as_mut() {
            directory.stacks.insert(ifindex, stack);
        }
    }

    /// Remove a veth DeviceStack identified by its nic_id.
    /// Silently returns if no matching stack exists.
    pub fn remove_veth_stack(&self, nic_id: u32) {
        let stack = {
            let mut directory = self.directory.lock();
            let Some(directory) = directory.as_mut() else {
                return;
            };
            let Some(stack) = directory.stacks.remove(&nic_id) else {
                return;
            };
            stack.state.store(STACK_DRAINING, Ordering::Release);
            directory
                .routes
                .retain(|_, entry| !entry.stack.ptr_eq(&Arc::downgrade(&stack)));
            stack
        };
        // 目录已先撤销全部 route；已取得 stack Arc 的访问者只能在栈锁内看到旧绑定并
        // 线性化于本次移除之前，后续访问会在目录阶段失败。
        stack.state.store(STACK_DEAD, Ordering::Release);
    }

    /// Sync an IP address into the smoltcp Interface of a DeviceStack.
    pub fn add_ip_to_stack(&self, ifindex: u32, cidr: IpCidr) {
        let Some(stack) = self.stack_arc(ifindex) else {
            return;
        };
        stack.inner.lock().iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(cidr);
        });
    }

    /// Remove an IP address from the smoltcp Interface of a DeviceStack.
    pub fn remove_ip_from_stack(&self, ifindex: u32, cidr: IpCidr) {
        let Some(stack) = self.stack_arc(ifindex) else {
            return;
        };
        stack.inner.lock().iface.update_ip_addrs(|addrs| {
            addrs.retain(|a| *a != cidr);
        });
    }

    pub fn replace_ip_addrs_on_stack(&self, ifindex: u32, cidr: IpCidr) {
        let Some(stack) = self.stack_arc(ifindex) else {
            return;
        };
        stack.inner.lock().iface.update_ip_addrs(|addrs| {
            addrs.clear();
            let _ = addrs.push(cidr);
        });
    }

    pub fn set_stack_mtu(&self, ifindex: u32, mtu: usize) {
        let Some(stack) = self.stack_arc(ifindex) else {
            return;
        };
        stack.inner.lock().iface.set_mtu(mtu);
    }

    pub fn transmit_on_stack(
        &self,
        ifindex: u32,
        bytes: &[u8],
    ) -> Result<isize, crate::utils::error::SyscallErr> {
        let stack = self
            .stack_arc(ifindex)
            .ok_or(crate::utils::error::SyscallErr::ENETDOWN)?;
        let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
        let mut inner = stack.inner.lock();
        let token = inner
            .device
            .transmit(timestamp)
            .ok_or(crate::utils::error::SyscallErr::ENETDOWN)?;
        token.consume(bytes.len(), |buffer| buffer.copy_from_slice(bytes));
        Ok(bytes.len() as isize)
    }

    pub fn tcp_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let stack = self.stack_arc(ifindex)?;
        let mut stack = stack.inner.lock();
        let socket = stack.sockets.get_mut::<tcp::Socket>(handler);
        Some(f(socket))
    }

    pub fn udp_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let stack = self.stack_arc(ifindex)?;
        let mut stack = stack.inner.lock();
        let socket = stack.sockets.get_mut::<udp::Socket>(handler);
        Some(f(socket))
    }

    pub fn raw_socket<T>(
        &self,
        handler: SocketHandle,
        ifindex: u32,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let stack = self.stack_arc(ifindex)?;
        let mut stack = stack.inner.lock();
        let socket = stack.sockets.get_mut::<raw::Socket>(handler);
        Some(f(socket))
    }

    /// Return the ifindex of every currently-registered DeviceStack.
    pub fn stack_ifindexes(&self) -> Vec<u32> {
        self.directory
            .lock()
            .as_ref()
            .map(|directory| directory.stacks.keys().copied().collect())
            .unwrap_or_default()
    }

    /// 返回 (tcp_count, udp_count, raw_count, pending_remove)
    pub fn socket_stats(&self) -> (usize, usize, usize, usize) {
        let tcp = crate::net::TCP_SOCKETS.lock().len();
        let raw = crate::net::RAW_SOCKETS.lock().len();
        let pending = TCP_SOCKETS_TO_REMOVE.lock().len() + UDP_SOCKETS_TO_REMOVE.lock().len();
        let stacks: Vec<_> = self
            .directory
            .lock()
            .as_ref()
            .map(|directory| directory.stacks.values().cloned().collect())
            .unwrap_or_default();
        let udp = stacks
            .iter()
            .map(|stack| {
                stack
                    .inner
                    .lock()
                    .sockets
                    .iter()
                    .filter(|(_, socket)| matches!(socket, smoltcp::socket::Socket::Udp(_)))
                    .count()
            })
            .sum();
        (tcp, udp, raw, pending)
    }

    /// Hard-IRQ publish-only network kick。
    ///
    /// 此函数的上界仅为两个原子更新；不得触碰目录、DeviceStack、WaitQueue
    /// 或 smoltcp 锁。
    pub fn try_poll_irq(&self) -> bool {
        self.kick_from_irq();
        true
    }
    /// 只尝试目标 DeviceStack；目录锁只用于取得 Arc，因此持有 stack A 不会阻塞 stack B。
    pub fn try_poll_stack(&self, ifindex: u32) -> bool {
        let Some(stack) = self.stack_arc(ifindex) else {
            return false;
        };
        *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock() = stack.ifindex;
        let mut stack_guard = match stack.inner.try_lock() {
            Some(guard) => guard,
            None => {
                // N2 忙时 worker 只记录下一 scheduler tick 的 retry，不能在此处重发
                // ticket；否则同一忙栈会驱动 worker 紧循环并饿死真正的锁持有者。
                self.poll.retry_armed.store(true, Ordering::Release);
                crate::task::perf::record_net_poll(false, true);
                #[cfg(feature = "net_perf_diag")]
                record_poll_perf(true, false, true, 0);
                return false;
            }
        };
        #[cfg(feature = "net_perf_diag")]
        let poll_start = crate::hal::get_time();
        let packet_frames = match &stack_guard.device {
            IfaceDevice::Veth(veth) => veth.inner.rx_queue.lock().iter().cloned().collect(),
            IfaceDevice::Lo(_) | IfaceDevice::Eth(_) => Vec::new(),
        };
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let DeviceStackInner {
            iface,
            device,
            sockets,
            ..
        } = &mut *stack_guard;
        let mut progressed = iface.poll(now, device, sockets);
        progressed |= capture_dhcp_event(&mut stack_guard);
        let dhcp_event = stack_guard
            .pending_dhcp_event
            .take()
            .map(|event| (stack.ifindex, event));
        let packets = drain_udp_packets(&mut stack_guard.sockets);
        drop(stack_guard);
        crate::net::socket::packet::deliver_veth_frame_snapshot(stack.ifindex, packet_frames);
        dispatch_udp_packets(packets);
        if let Some((ifindex, event)) = dhcp_event {
            commit_dhcp_event(ifindex, event);
        }
        // smoltcp 的 `poll()` 返回值不涵盖全部 socket readiness 状态转换；在 N2
        // 释放后统一刷新 pollee 并通知，避免把 event/epoll 唤醒带进 DeviceStack。
        crate::net::wake_tcp_waiters();
        crate::net::wake_raw_waiters();
        crate::net::wake_tcp_accept_waiters();
        crate::task::perf::record_net_poll(progressed, false);
        #[cfg(feature = "net_perf_diag")]
        record_poll_perf(
            true,
            progressed,
            false,
            crate::hal::get_time().wrapping_sub(poll_start),
        );
        progressed
    }

    /// 目录锁只用于克隆稳定的栈 Arc；真正的 smoltcp poll 从不持有目录锁。
    fn snapshot_stack_arcs(&self) -> Vec<Arc<DeviceStackCell<'a>>> {
        self.directory
            .lock()
            .as_ref()
            .map(|directory| directory.stacks.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 在 worker 的 task context 收集待删除 route。该步骤不持有任何 DeviceStack 锁。
    /// 返回值表示是否仍有 TCP close 需要定时推进。
    fn drain_pending_socket_removals(&self) -> bool {
        let udp_removes: Vec<_> = UDP_SOCKETS_TO_REMOVE.lock().drain(..).collect();
        for route in udp_removes {
            self.remove_routed(route);
        }

        let tcp_removes: Vec<_> = TCP_SOCKETS_TO_REMOVE.lock().drain(..).collect();
        for mut removal in tcp_removes {
            let closed = self
                .tcp_routed_socket(removal.route, |socket| {
                    if !removal.close_started {
                        if removal.abort_if_active && socket.is_active() {
                            socket.abort();
                        } else {
                            socket.close();
                        }
                        removal.close_started = true;
                    }
                    socket.state() == tcp::State::Closed
                })
                .unwrap_or(true);
            if closed {
                self.remove_routed(removal.route);
            } else if TimeSpec::now() >= removal.deadline {
                let aborted = self
                    .tcp_routed_socket(removal.route, |socket| socket.abort())
                    .is_some();
                if !aborted {
                    log::warn!(
                        "[net] TCP removal route {} disappeared before forced abort",
                        removal.route
                    );
                }
                self.remove_routed(removal.route);
            } else {
                let mut removals = TCP_SOCKETS_TO_REMOVE.lock();
                if let Some(existing) = removals
                    .iter_mut()
                    .find(|pending| pending.route == removal.route)
                {
                    existing.deadline = existing.deadline.min(removal.deadline);
                    existing.abort_if_active |= removal.abort_if_active;
                } else {
                    removals.push(removal);
                }
            }
        }
        !TCP_SOCKETS_TO_REMOVE.lock().is_empty()
    }

    /// TCP 仍处于 FIN_WAIT/TIME_WAIT 时，按 smoltcp 的 `poll_at()` 重装唯一的
    /// worker deadline；没有协议 deadline 时仍由每个 route 的强制回收 deadline
    /// 保证最终释放。任何目录、SocketSet 与 timer queue 锁都不重叠。
    fn rearm_tcp_cleanup_poll(&self) {
        let forced_deadline = {
            let removals = TCP_SOCKETS_TO_REMOVE.lock();
            removals.iter().map(|removal| removal.deadline).min()
        };
        let generation = self
            .poll
            .tcp_cleanup_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let Some(forced_deadline) = forced_deadline else {
            return;
        };

        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let protocol_deadline = self
            .snapshot_stack_arcs()
            .into_iter()
            .filter(|stack| stack.state.load(Ordering::Acquire) == STACK_ACTIVE)
            .filter_map(|stack| {
                let mut inner = stack.inner.try_lock()?;
                let DeviceStackInner { iface, sockets, .. } = &mut *inner;
                iface.poll_at(now, sockets)
                    .map(|deadline| TimeSpec::from_us(deadline.total_micros().max(0) as usize))
            })
            .min();
        let deadline = protocol_deadline
            .map(|deadline| deadline.min(forced_deadline))
            .unwrap_or(forced_deadline)
            .max(TimeSpec::now());
        crate::task::add_kernel_timer(
            crate::task::TimerAction::NetPoll { generation },
            deadline,
        );
    }

    /// 一轮 worker poll：每个 stack 只试拿一次锁；每次通知均由 `try_poll_stack()`
    /// 在释放 DeviceStack 后完成，严格保持 N2 -> N3 不反向嵌套。
    fn poll_each_stack_bounded(&self) {
        // A syscall/trap poll may have queued TX while local IRQs were off.
        // Only the scheduler worker (IRQ-enabled) is allowed to drain it.
        drain_deferred_tx();
        let tcp_cleanup_pending = self.drain_pending_socket_removals();
        for stack in self.snapshot_stack_arcs() {
            if stack.state.load(Ordering::Acquire) == STACK_ACTIVE {
                let _ = self.try_poll_stack(stack.ifindex);
            }
        }
        if tcp_cleanup_pending {
            self.rearm_tcp_cleanup_poll();
        } else {
            // 使所有已装载的 TCP cleanup deadline 失效；不会为已回收的 socket 留下
            // 额外 worker wakeup。
            self.poll
                .tcp_cleanup_generation
                .fetch_add(1, Ordering::AcqRel);
        }
    }

    /// 在当前任务上下文执行一次不等待的网络扫描。
    ///
    /// 该入口只用于 `O_NONBLOCK` 与零超时查询：每个 DeviceStack 都通过
    /// `try_lock()` 获取，忙栈只登记下一 tick 重试，因此调用时间有结构化上界。
    /// 调用方不得持有 socket、DeviceStack、fd table 或 EventWaitQueue 锁。
    pub fn poll_now(&self) {
        self.poll_each_stack_bounded();
    }

    /// IRQ producers only publish a deferred poll request; smoltcp remains in
    /// the CPU0 worker's task context and never runs from hard IRQ context.
    pub fn notify_rx_interrupt(&self) {
        let _ = self.try_poll_irq();
    }

    /// CPU0 专属的网络轮询 worker。
    ///
    /// 创建方在 `TaskStatus::New` 时将 affinity 固定为 BOOT_CPU_ID；每次醒来最多
    /// 消费两轮 pending 请求。第一轮清门后到来的请求由第二轮处理；第二轮之后
    /// 仍有新请求则保留 pending，交还 scheduler 后重新走等待协议，不在此处自旋。
    pub fn net_poll_worker(&self) {
        loop {
            match crate::task::WaitQueue::wait_event_interruptible(self.worker_wait(), || {
                self.poll.pending.load(Ordering::Acquire).then_some(0isize)
            }) {
                crate::task::WaitResult::Ready(_) => {}
                crate::task::WaitResult::Interrupted => {
                    let stopping = crate::task::current_task()
                        .map(|task| task.process.thread_must_exit(task.gettid()))
                        .unwrap_or(true);
                    if stopping {
                        crate::task::zombify_current_and_run_next();
                    }
                }
                crate::task::WaitResult::TimedOut => {}
            }

            for _ in 0..2 {
                // AcqRel 清门与 producer 的 AcqRel test-and-set 配对：清门后的新
                // 提交必定重新置 pending，因而不会丢失下一轮扫描请求。
                if !self.poll.pending.swap(false, Ordering::AcqRel) {
                    break;
                }
                // kernel worker 从调度器的 IRQ-off 边界进入。smoltcp 可能同步
                // 提交 VirtIO TX 并轮询 used ring，不应在整个等待期间屏蔽 timer/IPI。
                // hard IRQ 仍只发布原子 pending/deferred 状态，不会重入
                // DeviceStack、VirtIO 或 WaitQueue 锁域。
                crate::hal::with_local_interrupts_enabled(|| self.poll_each_stack_bounded());
                // 受控窗口已关闭，且本轮所有网络锁均已释放；在此统一兑现窗口内
                // 累积的 timer/IPI 调度请求，禁止在网络临界区中途切换任务。
                crate::task::run_task_safe_point();
            }
        }
    }

    /// 旧的直接 SocketHandle API 只服务于未路由内部 socket；公开 Inet socket 必须
    /// 使用 route ID，从而在 DeviceStack 内重验绑定。
    pub fn remove(&self, handler: SocketHandle, ifindex: u32) {
        let Some(stack) = self.stack_arc(ifindex) else {
            return;
        };
        let mut stack_guard = stack.inner.lock();
        let removed = stack_guard.sockets.remove(handler);
        drop(stack_guard);
        drop(removed);
    }

    fn add_routed_socket_on_stack<T>(
        &self,
        proto: InetProtocol,
        socket: T,
        ifindex: u32,
    ) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let stack = self.stack_arc(ifindex)?;
        let route = RouteSocketHandle(
            self.next_route_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .ok()?,
        );
        {
            let mut inner = stack.inner.lock();
            if stack.state.load(Ordering::Acquire) != STACK_ACTIVE {
                return None;
            }
            let handle = inner.sockets.add(socket);
            inner.bindings.insert(
                route,
                LocalSocketBinding {
                    handle,
                    protocol: proto,
                },
            );
        }
        let published = {
            let mut directory = self.directory.lock();
            let mut published = false;
            if let Some(directory) = directory.as_mut() {
                if let Some(current) = directory.stacks.get(&ifindex) {
                    // local binding 建立期间设备可能被另一个 CPU 删除再重建；只有目录
                    // 中仍是同一个栈且状态仍为 ACTIVE，才允许把 route 发布给读者。
                    if Arc::ptr_eq(current, &stack)
                        && stack.state.load(Ordering::Acquire) == STACK_ACTIVE
                    {
                        directory.routes.insert(
                            route,
                            RouteDirectoryEntry {
                                stack: Arc::downgrade(&stack),
                                protocol: proto,
                                state: RouteState::Active,
                            },
                        );
                        published = true;
                    }
                }
            }
            published
        };
        if published {
            return Some(route);
        }
        // 设备在 local binding 建立后被撤销时，读者尚未看到该 route；撤销只在栈锁内
        // 完成，析构在锁外执行。
        let removed = {
            let mut inner = stack.inner.lock();
            inner
                .bindings
                .remove(&route)
                .map(|binding| inner.sockets.remove(binding.handle))
        };
        drop(removed);
        None
    }

    pub fn add_routed_socket<T>(&self, proto: InetProtocol, socket: T) -> Option<RouteSocketHandle>
    where
        T: AnySocket<'a>,
    {
        let ifindex = net_core::default_iface()
            .map(|iface| iface.ifindex)
            .unwrap_or(1);
        self.add_routed_socket_on_stack(proto, socket, ifindex)
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
        self.add_routed_socket_on_stack(proto, socket, ifindex)
    }

    pub fn tcp_routed_socket<T>(
        &self,
        route: RouteSocketHandle,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let stack = self.active_route_stack(route, InetProtocol::Tcp)?;
        let mut inner = stack.inner.lock();
        let binding = *inner.bindings.get(&route)?;
        if binding.protocol != InetProtocol::Tcp {
            return None;
        }
        Some(f(inner.sockets.get_mut::<tcp::Socket>(binding.handle)))
    }

    pub fn udp_routed_socket<T>(
        &self,
        route: RouteSocketHandle,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let stack = self.active_route_stack(route, InetProtocol::Udp)?;
        let mut inner = stack.inner.lock();
        let binding = *inner.bindings.get(&route)?;
        if binding.protocol != InetProtocol::Udp {
            return None;
        }
        Some(f(inner.sockets.get_mut::<udp::Socket>(binding.handle)))
    }

    pub fn raw_routed_socket<T>(
        &self,
        route: RouteSocketHandle,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let stack = self.active_route_stack(route, InetProtocol::Raw)?;
        let mut inner = stack.inner.lock();
        let binding = *inner.bindings.get(&route)?;
        if binding.protocol != InetProtocol::Raw {
            return None;
        }
        Some(f(inner.sockets.get_mut::<raw::Socket>(binding.handle)))
    }

    pub fn tcp_connect(
        &self,
        route: RouteSocketHandle,
        remote: smoltcp::wire::IpEndpoint,
        local: smoltcp::wire::IpEndpoint,
    ) -> Option<Result<(), smoltcp::socket::tcp::ConnectError>> {
        let stack = self.active_route_stack(route, InetProtocol::Tcp)?;
        let mut inner = stack.inner.lock();
        let binding = *inner.bindings.get(&route)?;
        if binding.protocol != InetProtocol::Tcp {
            return None;
        }
        let DeviceStackInner { iface, sockets, .. } = &mut *inner;
        let context = iface.context();
        Some(
            sockets
                .get_mut::<tcp::Socket>(binding.handle)
                .connect(context, remote, local),
        )
    }

    pub fn remove_routed(&self, route: RouteSocketHandle) {
        let entry = self.directory.lock().as_mut().and_then(|directory| {
            let entry = directory.routes.get_mut(&route)?;
            entry.state = RouteState::Draining;
            directory.routes.remove(&route)
        });
        let Some(entry) = entry else {
            log::warn!("[net] route {} was absent during SocketSet removal", route);
            return;
        };
        let Some(stack) = entry.stack.upgrade() else {
            log::warn!("[net] route {} lost its DeviceStack during SocketSet removal", route);
            return;
        };
        let removed = {
            let mut inner = stack.inner.lock();
            inner
                .bindings
                .remove(&route)
                .map(|binding| inner.sockets.remove(binding.handle))
        };
        if removed.is_none() {
            log::warn!("[net] route {} had no SocketSet binding during removal", route);
        }
        drop(removed);
    }

    pub fn rebind_routed_udp(
        &self,
        route: RouteSocketHandle,
        new_ifindex: u32,
    ) -> Option<RouteSocketHandle> {
        let source = {
            let mut directory = self.directory.lock();
            let entry = directory.as_mut()?.routes.get_mut(&route)?;
            if entry.state != RouteState::Active || entry.protocol != InetProtocol::Udp {
                return None;
            }
            let source = entry.stack.upgrade()?;
            if source.ifindex == new_ifindex {
                return Some(route);
            }
            entry.state = RouteState::Migrating;
            source
        };
        let Some(target) = self.stack_arc(new_ifindex) else {
            if let Some(entry) = self
                .directory
                .lock()
                .as_mut()
                .and_then(|directory| directory.routes.get_mut(&route))
            {
                entry.stack = Arc::downgrade(&source);
                entry.state = RouteState::Active;
            }
            return None;
        };
        if target.state.load(Ordering::Acquire) != STACK_ACTIVE {
            if let Some(entry) = self
                .directory
                .lock()
                .as_mut()
                .and_then(|directory| directory.routes.get_mut(&route))
            {
                entry.stack = Arc::downgrade(&source);
                entry.state = RouteState::Active;
            }
            return None;
        }
        let rx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 1024],
            vec![0u8; crate::net::MAX_BUFFER_SIZE],
        );
        let tx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 1024],
            vec![0u8; crate::net::MAX_BUFFER_SIZE],
        );
        let replacement = udp::Socket::new(rx_buf, tx_buf);
        let removed = {
            let mut inner = source.inner.lock();
            let binding = inner.bindings.remove(&route)?;
            if binding.protocol != InetProtocol::Udp {
                return None;
            }
            inner.sockets.remove(binding.handle)
        };
        drop(removed);
        let target_handle = {
            let mut inner = target.inner.lock();
            if target.state.load(Ordering::Acquire) != STACK_ACTIVE {
                return None;
            }
            let handle = inner.sockets.add(replacement);
            inner.bindings.insert(
                route,
                LocalSocketBinding {
                    handle,
                    protocol: InetProtocol::Udp,
                },
            );
            handle
        };
        let mut directory = self.directory.lock();
        let entry = directory.as_mut()?.routes.get_mut(&route)?;
        if entry.state != RouteState::Migrating {
            return None;
        }
        entry.stack = Arc::downgrade(&target);
        entry.state = RouteState::Active;
        let _ = target_handle;
        Some(route)
    }
}

/// 内核任务入口：由 boot 在创建期固定到 CPU0，随后永久消费合并后的 poll 请求。
pub fn net_poll_worker() {
    NET_INTERFACE.net_poll_worker()
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
