use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use alloc::sync::{Arc, Weak};
use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use crate::drivers::NET_DEVICE;
use lazy_static::*;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{DeviceCapabilities, Loopback, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use spin::{Mutex, RwLock};

pub use crate::net::iface::{
    DeviceKind, Iface, IfaceCommon, SmoltcpDeviceAccess,
};
use crate::task::NetNamespace;

// ---------------------------------------------------------------------------
// Flags (re-exported for compatibility)
// ---------------------------------------------------------------------------

pub const IFF_UP: u32 = 0x1;
pub const IFF_BROADCAST: u32 = 0x2;
pub const IFF_LOOPBACK: u32 = 0x8;
pub const IFF_RUNNING: u32 = 0x40;
pub const IFF_NOARP: u32 = 0x80;
pub const IFF_MULTICAST: u32 = 0x1000;

pub const IF_OPER_UP: u8 = 6;

// ---------------------------------------------------------------------------
// DeviceEntry — thin wrapper around Arc<dyn Iface>
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DeviceEntry {
    pub ifindex: u32,
    pub iface: Arc<dyn Iface>,
}

impl fmt::Debug for DeviceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceEntry")
            .field("ifindex", &self.ifindex)
            .field("name", &self.iface.iface_name())
            .field("kind", &self.iface.kind())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// DHCP / gateway globals (keep — they are per-kernel state, not per-iface list)
// ---------------------------------------------------------------------------

lazy_static! {
    /// DHCP-assigned IPv4 CIDR for eth0 (set after DHCP probe completes)
    pub static ref ETH0_CIDR: Mutex<Option<IpCidr>> = Mutex::new(None);
    /// Default gateway (set after DHCP probe completes)
    pub static ref DEFAULT_GW: Mutex<Option<Ipv4Address>> = Mutex::new(None);
}

// ---------------------------------------------------------------------------
// Global ifindex counter (shared across all namespaces)
// ---------------------------------------------------------------------------

static NEXT_IFINDEX: AtomicU32 = AtomicU32::new(3);

pub fn next_ifindex() -> u32 {
    NEXT_IFINDEX.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// current_netns() helper
// ---------------------------------------------------------------------------

pub fn current_netns() -> Arc<NetNamespace> {
    match crate::task::current_task() {
        Some(t) => t.process.net(),
        None => crate::task::INIT_NET_NAMESPACE.clone(),
    }
}

// ---------------------------------------------------------------------------
// NetDeviceEntry — concrete Iface implementation
// ---------------------------------------------------------------------------

/// Concrete implementation of [`Iface`] for device registry entries.
///
/// Holds per-interface metadata (name, flags, IPs, MAC, etc.) plus a dummy
/// smoltcp context.  Real protocol processing is handled by
/// [`crate::net::config::NetInterface`]; the smoltcp fields here exist only
/// to satisfy the [`Iface`] trait and are never polled.
pub struct NetDeviceEntry {
    // --- metadata fields (mirror the old DeviceEntry) ---
    nic_id: AtomicUsize,
    name: Mutex<String>,   // thread-safe name storage
    flags: AtomicU32,
    mtu: AtomicUsize,
    ip_addrs: Mutex<Vec<IpCidr>>,
    hwaddr: [u8; 6],
    kind: DeviceKind,
    peer_ifindex: Option<usize>,
    operstate: AtomicU32,

    // --- smoltcp dummy context (satisfies Iface trait interface) ---
    smoltcp_iface: Mutex<Interface>,
    sockets: Mutex<SocketSet<'static>>,
    net_namespace: RwLock<Option<Weak<NetNamespace>>>,
}

impl fmt::Debug for NetDeviceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetDeviceEntry")
            .field("name", &*self.name.lock())
            .field("kind", &self.kind)
            .finish()
    }
}

impl NetDeviceEntry {
    /// Create a new entry with all metadata fields initialised.
    pub fn new(
        name: String,
        kind: DeviceKind,
        hwaddr: [u8; 6],
        mtu: usize,
        flags: u32,
        ip_addrs: Vec<IpCidr>,
        peer_ifindex: Option<usize>,
        operstate: u32,
    ) -> Self {
        // Create a dummy smoltcp Interface for trait compatibility.
        // The real protocol processing is done by NetInterface's stacks.
        let mut lo = Loopback::new(Medium::Ip);
        let config = Config::new(HardwareAddress::Ip);
        let smoltcp_iface = Interface::new(config, &mut lo, Instant::from_millis(0));
        let sockets = SocketSet::new(vec![]);

        NetDeviceEntry {
            nic_id: AtomicUsize::new(0),
            name: Mutex::new(name),
            flags: AtomicU32::new(flags),
            mtu: AtomicUsize::new(mtu),
            ip_addrs: Mutex::new(ip_addrs),
            hwaddr,
            kind,
            peer_ifindex,
            operstate: AtomicU32::new(operstate),
            smoltcp_iface: Mutex::new(smoltcp_iface),
            sockets: Mutex::new(sockets),
            net_namespace: RwLock::new(None),
        }
    }

    /// Set the nic_id after construction (assigned from NEXT_IFINDEX).
    pub fn set_nic_id(&self, id: usize) {
        self.nic_id.store(id, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Iface trait impl for NetDeviceEntry
// ---------------------------------------------------------------------------

impl Iface for NetDeviceEntry {
    fn nic_id(&self) -> usize {
        self.nic_id.load(Ordering::Relaxed)
    }

    fn iface_name(&self) -> String {
        self.name.lock().clone()
    }

    fn set_iface_name(&self, name: &str) {
        *self.name.lock() = String::from(name);
    }

    fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    fn set_flags(&self, flags: u32) {
        self.flags.store(flags, Ordering::Relaxed);
    }

    fn mtu(&self) -> usize {
        self.mtu.load(Ordering::Relaxed)
    }

    fn set_mtu(&self, mtu: usize) {
        self.mtu.store(mtu, Ordering::Relaxed);
    }

    fn ip_addrs(&self) -> Vec<IpCidr> {
        self.ip_addrs.lock().clone()
    }

    fn add_ip_addr(&self, addr: IpCidr) {
        self.ip_addrs.lock().push(addr);
    }

    fn del_ip_addr(&self, addr: IpCidr) {
        self.ip_addrs.lock().retain(|a| *a != addr);
    }

    fn mac(&self) -> [u8; 6] {
        self.hwaddr
    }

    fn kind(&self) -> DeviceKind {
        self.kind
    }

    fn peer_ifindex(&self) -> Option<usize> {
        self.peer_ifindex
    }

    fn common(&self) -> &IfaceCommon {
        // TODO (Wave 2): implement properly when veth/loopback concrete types
        // are created. NetDeviceEntry is never polled via this path.
        panic!("NetDeviceEntry::common() — not supported (use NetInterface stacks)")
    }

    fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess {
        // TODO (Wave 2): implement properly when veth/loopback concrete types
        // are created. NetDeviceEntry is never polled via this path.
        panic!("NetDeviceEntry::as_smoltcp_device() — not supported (use NetInterface stacks)")
    }
}

// ---------------------------------------------------------------------------
// Device registry operations (routed through current netns)
// ---------------------------------------------------------------------------

/// Register a new device in the current network namespace.
pub fn add_device(iface: Arc<dyn Iface>) {
    let ns = current_netns();
    // Track which namespace owns this device, so that delete/move
    // can find the correct namespace even after netns switches.
    *iface.common().net_namespace.write() = Some(Arc::downgrade(&ns));
    ns.add_device(iface);
}

/// Remove a device from the current network namespace by nic_id.
pub fn remove_device(nic_id: usize) {
    current_netns().remove_device(nic_id);
}

/// Find a device by name in the current network namespace.
pub fn find_by_name(name: &str) -> Option<DeviceEntry> {
    let ns = current_netns();
    ns.device_by_name(name).map(|iface| DeviceEntry {
        ifindex: iface.nic_id() as u32,
        iface,
    })
}

/// Find a device by ifindex in the current network namespace.
pub fn find_by_index(idx: u32) -> Option<DeviceEntry> {
    let ns = current_netns();
    ns.device_by_index(idx as usize).map(|iface| DeviceEntry {
        ifindex: idx,
        iface,
    })
}

// ---------------------------------------------------------------------------
// init() — register lo + eth0 in the init namespace
// ---------------------------------------------------------------------------

/// Initialize network device registry (idempotent).
///
/// Registers loopback (ifindex=1) and, if a NIC is present, eth0 (ifindex=2)
/// into the init network namespace.
pub fn init() {
    let ns = crate::task::net_namespace::INIT_NET_NAMESPACE.clone();
    {
        let devs = ns.device_list.lock();
        if !devs.is_empty() {
            return; // already initialized
        }
    }

    // --- loopback ---
    {
        let lo = Arc::new(NetDeviceEntry::new(
            String::from("lo"),
            DeviceKind::Loopback,
            [0u8; 6],
            65536,
            IFF_UP | IFF_LOOPBACK | IFF_RUNNING,
            vec![
                IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8),
                IpCidr::new(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1), 128),
            ],
            None,
            IF_OPER_UP as u32,
        ));
        lo.set_nic_id(1);
        ns.add_device(lo);
    }
    log::info!("[net_core] registered lo (ifindex=1)");

    // --- eth0 (only if NIC is present) ---
    let net_guard = NET_DEVICE.lock();
    if let Some(dev) = net_guard.as_ref() {
        let mac = dev.mac_address();
        drop(net_guard);

        let eth0 = Arc::new(NetDeviceEntry::new(
            String::from("eth0"),
            DeviceKind::Ethernet,
            mac,
            1500,
            IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST,
            vec![], // IP set by DHCP later
            None,
            IF_OPER_UP as u32,
        ));
        eth0.set_nic_id(2);
        ns.add_device(eth0);
        log::info!("[net_core] registered eth0 (ifindex=2, no static IP)");
    } else {
        drop(net_guard);
    }
}

// ---------------------------------------------------------------------------
// Convenience helpers (routed through current netns)
// ---------------------------------------------------------------------------

/// Return the default network interface: eth0 if registered, otherwise lo.
pub fn default_iface() -> Option<DeviceEntry> {
    let ns = current_netns();
    let list = ns.device_list.lock();
    list.values()
        .find(|iface| iface.iface_name() == "eth0")
        .or_else(|| list.values().find(|iface| iface.iface_name() == "lo"))
        .map(|iface| DeviceEntry {
            ifindex: iface.nic_id() as u32,
            iface: iface.clone(),
        })
}

/// Return the loopback interface "lo".
pub fn loopback_iface() -> Option<DeviceEntry> {
    let ns = current_netns();
    ns.device_by_name("lo").map(|iface| DeviceEntry {
        ifindex: iface.nic_id() as u32,
        iface,
    })
}

/// Return the default gateway address (set by DHCP), otherwise None.
pub fn default_gateway() -> Option<Ipv4Address> {
    *DEFAULT_GW.lock()
}

/// Set the DHCP-assigned IPv4 address for eth0.
pub fn set_eth0_ipv4(cidr: IpCidr) {
    *ETH0_CIDR.lock() = Some(cidr);
    let ns = current_netns();
    let list = ns.device_list.lock();
    if let Some(eth0) = list.values().find(|iface| iface.iface_name() == "eth0") {
        // Clear old IPs, add the new one
        for old in eth0.ip_addrs() {
            eth0.del_ip_addr(old);
        }
        eth0.add_ip_addr(cidr);
    }
}

/// Return the DHCP-assigned eth0 IPv4 CIDR, if any.
pub fn eth0_ipv4_cidr() -> Option<IpCidr> {
    *ETH0_CIDR.lock()
}

/// Set the default gateway address (from DHCP).
pub fn set_default_gateway(gw: Option<Ipv4Address>) {
    *DEFAULT_GW.lock() = gw;
}

/// Check if the given address belongs to any local interface.
pub fn is_local_addr(addr: Ipv4Address) -> bool {
    let ip = IpAddress::Ipv4(addr);
    let ns = current_netns();
    let list = ns.device_list.lock();
    list.values().any(|iface| {
        iface.ip_addrs().iter().any(|c| c.address() == ip)
    })
}

/// Find the ifindex of the device that owns the given local IP address.
///
/// For loopback (127.x.x.x) or unspecified (INADDR_ANY), returns the loopback
/// device's ifindex.  For specific addresses, searches the device list by IP.
/// Falls back to the loopback device if no match is found.
pub fn ifindex_for_local_addr(addr: Option<IpAddress>) -> u32 {
    let ns = current_netns();
    let list = ns.device_list.lock();
    match addr {
        None => list
            .values()
            .find(|iface| iface.iface_name() == "lo")
            .map(|iface| iface.nic_id() as u32)
            .unwrap_or(1),
        Some(IpAddress::Ipv4(ip)) if ip.is_loopback() => list
            .values()
            .find(|iface| iface.iface_name() == "lo")
            .map(|iface| iface.nic_id() as u32)
            .unwrap_or(1),
        Some(ip) => list
            .values()
            .find(|iface| iface.ip_addrs().iter().any(|c| c.address() == ip))
            .map(|iface| iface.nic_id() as u32)
            .unwrap_or_else(|| {
                list.values()
                    .find(|iface| iface.iface_name() == "lo")
                    .map(|iface| iface.nic_id() as u32)
                    .unwrap_or(1)
            }),
    }
}

/// Return the local port range for ephemeral ports (32768–60999 on Linux).
pub fn local_port_range() -> (u16, u16) {
    (32_768, 60_999)
}

/// Return the IP address of the interface identified by `ifindex`.
pub fn iface_ip(ifindex: u32) -> Option<IpAddress> {
    let ns = current_netns();
    let list = ns.device_list.lock();
    list.values()
        .find(|iface| iface.nic_id() == ifindex as usize)
        .and_then(|iface| iface.ip_addrs().first().map(|c| c.address()))
}
