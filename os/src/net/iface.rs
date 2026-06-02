//! Network interface abstraction layer.
//!
//! Defines the `Iface` trait (unified interface for all network devices),
//! `IfaceCommon` (shared per-interface state including smoltcp `Interface` and
//! `SocketSet`), and `SmoltcpDeviceAccess` (device-level adapter for smoltcp
//! integration with `&self`-shared access).

use alloc::string::String;
use alloc::sync::Weak;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU32, AtomicUsize};
use smoltcp::iface::{Interface, SocketSet};
use smoltcp::phy::DeviceCapabilities;
use smoltcp::time::Instant;
use smoltcp::wire::IpCidr;
use spin::{Mutex, RwLock};

use crate::task::NetNamespace;

// ---------------------------------------------------------------------------
// DeviceKind
// ---------------------------------------------------------------------------

/// Network device kind / link-layer type.
///
/// Canonical definition; `net_core.rs` will re-export / import from here
/// post-migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Loopback,
    Ethernet,
    Veth,
}

// ---------------------------------------------------------------------------
// Iface trait
// ---------------------------------------------------------------------------

/// Unified trait for all network interfaces.
///
/// Concrete implementations: loopback, virtio-ethernet, veth.
/// Each implementation typically wraps an `IfaceCommon` plus a device-specific
/// driver struct that implements [`SmoltcpDeviceAccess`].
pub trait Iface: Send + Sync + fmt::Debug {
    /// Unique per-interface numeric identifier (assigned at registration time
    /// from the global `NEXT_IFINDEX` counter).
    fn nic_id(&self) -> usize;

    /// Human-readable interface name, e.g. `"lo"`, `"eth0"`, `"veth0"`.
    fn iface_name(&self) -> String;

    /// Rename the interface. Caller must verify uniqueness within the namespace.
    fn set_iface_name(&self, name: &str);

    /// Interface flags (IFF_UP, IFF_RUNNING, IFF_BROADCAST, etc.).
    fn flags(&self) -> u32;

    /// Update interface flags. Implementations should synchronise smoltcp state
    /// (e.g. enable/disable the interface in `Interface`).
    fn set_flags(&self, flags: u32);

    /// Maximum transmission unit.
    fn mtu(&self) -> usize;

    /// Update MTU. Implementations should synchronise smoltcp capabilities.
    fn set_mtu(&self, mtu: usize);

    /// All currently assigned IP addresses (CIDR format).
    fn ip_addrs(&self) -> Vec<IpCidr>;

    /// Add an IP address and, if applicable, a local subnet route.
    fn add_ip_addr(&self, addr: IpCidr);

    /// Remove an IP address and any associated local route.
    fn del_ip_addr(&self, addr: IpCidr);

    /// Hardware (MAC) address, 6 bytes.
    fn mac(&self) -> [u8; 6];

    /// Device kind.
    fn kind(&self) -> DeviceKind;

    /// If this is one end of a veth pair, return the peer's `nic_id`.
    /// Returns `None` for non-veth interfaces.
    fn peer_ifindex(&self) -> Option<usize>;

    /// Access the shared per-interface state (metadata + smoltcp internals).
    fn common(&self) -> &IfaceCommon;

    /// Access the smoltcp device adapter for polling / capability inspection.
    ///
    /// Only object-safe methods (`poll`, `capabilities`) are callable through
    /// the returned trait object; `receive` / `transmit` require the concrete
    /// type and are used internally by `Interface::poll`.
    fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess;
}

// ---------------------------------------------------------------------------
// IfaceCommon — shared per-interface state
// ---------------------------------------------------------------------------

/// Shared per-interface state.
///
/// Holds all metadata that was previously in `net_core::DeviceEntry`, plus the
/// smoltcp `Interface` and `SocketSet` needed for protocol processing.
///
/// The smoltcp `Interface` is *not* generic over the device type — the device
/// is passed at [`Interface::poll`] call time.  This allows `IfaceCommon` to
/// be stored without knowing the concrete device type at compile time.
pub struct IfaceCommon {
    /// Unique per-interface ID (assigned from global `NEXT_IFINDEX`).
    pub nic_id: AtomicUsize,

    /// Interface name, e.g. `"lo"`, `"eth0"`.
    pub name: RwLock<String>,

    /// Interface flags (IFF_UP | IFF_RUNNING | ...).
    pub flags: AtomicU32,

    /// Maximum transmission unit.
    pub mtu: AtomicUsize,

    /// IP addresses with CIDR prefixes.
    pub ip_addrs: Mutex<Vec<IpCidr>>,

    /// Hardware (MAC) address.
    pub hwaddr: [u8; 6],

    /// Device kind.
    pub kind: DeviceKind,

    /// For veth: peer `nic_id`. `None` otherwise.
    pub peer_ifindex: Option<usize>,

    /// Smoltcp protocol engine (one per interface).
    ///
    /// The device is *not* stored inside `Interface` — it is passed to
    /// `Interface::poll()` at runtime, which is why we can store it here
    /// without a generic type parameter.
    pub smoltcp_iface: Mutex<Interface>,

    /// Smoltcp socket set (one per interface).
    pub sockets: Mutex<SocketSet<'static>>,

    /// Which network namespace this interface belongs to.
    ///
    /// Uses `Weak` to avoid reference cycles (the namespace owns
    /// `Arc<Iface>` entries, and the iface points back to its namespace).
    pub net_namespace: RwLock<Option<Weak<NetNamespace>>>,
}

impl IfaceCommon {
    /// Create a new `IfaceCommon`.
    ///
    /// `nic_id` is initialised to 0; the registry will assign the real value
    /// after construction (from the global `NEXT_IFINDEX` counter).
    pub fn new(
        name: String,
        kind: DeviceKind,
        hwaddr: [u8; 6],
        mtu: usize,
        smoltcp_iface: Interface,
        sockets: SocketSet<'static>,
    ) -> Self {
        Self {
            nic_id: AtomicUsize::new(0),
            name: RwLock::new(name),
            flags: AtomicU32::new(0),
            mtu: AtomicUsize::new(mtu),
            ip_addrs: Mutex::new(Vec::new()),
            hwaddr,
            kind,
            peer_ifindex: None,
            smoltcp_iface: Mutex::new(smoltcp_iface),
            sockets: Mutex::new(sockets),
            net_namespace: RwLock::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// SmoltcpDeviceAccess trait
// ---------------------------------------------------------------------------

/// Object-safe device adapter for network poll loop integration.
///
/// Unlike smoltcp's `Device` trait (which requires `&mut self` and provides
/// token-based `receive`/`transmit`), this trait uses `&self` so
/// implementations can be shared behind `Arc`.  Internal mutability (typically
/// a spinlock) is expected in each implementation.
///
/// Concrete device types additionally implement [`smoltcp::phy::Device`] for
/// the token-based I/O that `Interface::poll` requires.  `SmoltcpDeviceAccess`
/// is the object-safe projection used by the poll loop via
/// [`Iface::as_smoltcp_device`].
pub trait SmoltcpDeviceAccess: Send + Sync {
    /// Poll the device and process pending frames.
    ///
    /// Called periodically from the network poll loop.
    fn poll(&self, timestamp: Instant) -> core::result::Result<(), ()>;

    /// Device capabilities (medium, max MTU, checksum offload, etc.).
    fn capabilities(&self) -> DeviceCapabilities;
}
