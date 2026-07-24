//! Veth (Virtual Ethernet) — paired network endpoints connected via in-memory queues.
//!
//! Each [`VethInterface`] implements the [`Iface`](crate::net::iface::Iface) trait,
//! backed by an internal [`IfaceCommon`](crate::net::iface::IfaceCommon). Two
//! endpoints can be interconnected so that a packet transmitted on one appears
//! in the rx_queue of its peer.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::Ordering;

use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress};
use spin::Mutex;

use crate::net::iface::{DeviceKind, Iface, IfaceCommon, SmoltcpDeviceAccess};
use crate::net::net_core;

/// Generate a locally-administered unicast MAC address from a NIC ID.
/// Format: `02:00:00:00:XX:YY` where `XXYY` = `nic_id` in big-endian.
pub fn generate_mac(nic_id: u32) -> [u8; 6] {
    [0x02, 0x00, 0x00, 0x00, (nic_id >> 8) as u8, nic_id as u8]
}

// ── Veth ───────────────────────────────────────────────────────────────

/// Maximum number of packets allowed in the rx_queue.
/// When reached, new incoming packets are silently dropped to prevent
/// unbounded memory growth (VecDeque reallocation OOM).
const MAX_VETH_QUEUE_LEN: usize = 4096;

/// Initial rx_queue capacity to avoid excessive early reallocations
/// while keeping memory footprint small.
const VETH_INITIAL_QUEUE_CAP: usize = 64;

pub struct Veth {
    pub rx_queue: Mutex<VecDeque<Vec<u8>>>,
    pub peer: Mutex<Weak<VethInterface>>,
}

impl Veth {
    pub fn new() -> Self {
        Self {
            rx_queue: Mutex::new(VecDeque::with_capacity(VETH_INITIAL_QUEUE_CAP)),
            peer: Mutex::new(Weak::new()),
        }
    }
}

// ── VethDriver ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct VethDriver {
    pub inner: Arc<Veth>,
}

impl VethDriver {
    pub fn new(inner: Arc<Veth>) -> Self {
        Self { inner }
    }
}

impl SmoltcpDeviceAccess for VethDriver {
    fn poll(&self, _timestamp: Instant) -> core::result::Result<(), ()> {
        Ok(())
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }
}

impl Device for VethDriver {
    type RxToken<'a> = VethRxToken;
    type TxToken<'a> = VethTxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let peer_veth = self.inner.peer.lock().upgrade().map(|p| p.data.clone());
        self.inner.rx_queue.lock().pop_front().map(|buf| {
            let rx = VethRxToken(buf);
            let tx = VethTxToken { peer_veth };
            (rx, tx)
        })
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let peer_veth = self.inner.peer.lock().upgrade().map(|p| p.data.clone());
        Some(VethTxToken { peer_veth })
    }
}

// ── VethRxToken / VethTxToken ─────────────────────────────────────────

pub struct VethRxToken(pub Vec<u8>);

impl RxToken for VethRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let ifindex = *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock();
        crate::net::neighbour::try_capture_arp_reply(&self.0, ifindex);
        f(&mut self.0)
    }
}

pub struct VethTxToken {
    peer_veth: Option<Arc<Veth>>,
}

impl TxToken for VethTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        if let Some(peer) = &self.peer_veth {
            let mut rx_queue = peer.rx_queue.lock();
            if rx_queue.len() >= MAX_VETH_QUEUE_LEN {
                log::warn!(
                    "[veth] rx_queue full ({} packets), dropping packet",
                    MAX_VETH_QUEUE_LEN
                );
            } else {
                rx_queue.push_back(buf);
            }
        }
        result
    }
}

// ── VethInterface ─────────────────────────────────────────────────────

pub struct VethInterface {
    pub data: Arc<Veth>,
    driver: VethDriver,
    common: IfaceCommon,
}

impl fmt::Debug for VethInterface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VethInterface")
            .field("name", &*self.common.name.read())
            .field("nic_id", &self.common.nic_id)
            .finish()
    }
}

impl VethInterface {
    pub fn new(name: &str, nic_id: u32) -> Arc<Self> {
        let data = Arc::new(Veth::new());
        let mut driver = VethDriver::new(data.clone());
        let mac = generate_mac(nic_id);
        let now = Instant::from_millis(crate::timer::current_time_duration().as_millis() as i64);
        let config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        let smoltcp_iface = Interface::new(config, &mut driver, now);
        let sockets = SocketSet::new(vec![]);

        let common = IfaceCommon::new(
            String::from(name),
            DeviceKind::Veth,
            mac,
            1500,
            smoltcp_iface,
            sockets,
        );
        common.nic_id.store(nic_id as usize, Ordering::Relaxed);

        Arc::new(Self {
            data,
            driver,
            common,
        })
    }
}

impl Iface for VethInterface {
    fn nic_id(&self) -> usize {
        self.common.nic_id.load(Ordering::Relaxed)
    }

    fn iface_name(&self) -> String {
        self.common.name.read().clone()
    }

    fn set_iface_name(&self, name: &str) {
        *self.common.name.write() = String::from(name);
    }

    fn flags(&self) -> u32 {
        self.common.flags.load(Ordering::Relaxed)
    }

    fn set_flags(&self, flags: u32) {
        self.common.flags.store(flags, Ordering::Relaxed);
    }

    fn mtu(&self) -> usize {
        self.common.mtu.load(Ordering::Relaxed)
    }

    fn set_mtu(&self, mtu: usize) {
        self.common.mtu.store(mtu, Ordering::Relaxed);
    }

    fn ip_addrs(&self) -> Vec<smoltcp::wire::IpCidr> {
        self.common.ip_addrs.lock().clone()
    }

    fn add_ip_addr(&self, addr: smoltcp::wire::IpCidr) {
        self.common.ip_addrs.lock().push(addr);
    }

    fn del_ip_addr(&self, addr: smoltcp::wire::IpCidr) {
        self.common.ip_addrs.lock().retain(|a| *a != addr);
    }

    fn mac(&self) -> [u8; 6] {
        self.common.hwaddr
    }

    fn kind(&self) -> DeviceKind {
        self.common.kind
    }

    fn peer_ifindex(&self) -> Option<usize> {
        self.common.peer_ifindex
    }

    fn common(&self) -> &IfaceCommon {
        &self.common
    }

    fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess {
        &self.driver
    }
}

// ── veth_pair_delete ───────────────────────────────────────────────────

/// Delete a veth pair given one end. Removes both ends from the device
/// registry and NET_INTERFACE stacks.
///
/// # Safety
///
/// Caller MUST guarantee that `iface` is actually a [`VethInterface`]
/// (i.e. `iface.kind() == DeviceKind::Veth`). Misuse will cause UB.
pub fn veth_pair_delete(iface: alloc::sync::Arc<dyn crate::net::iface::Iface>) {
    let veth_iface: &VethInterface =
        unsafe { &*(alloc::sync::Arc::as_ptr(&iface) as *const VethInterface) };

    let own_nic = veth_iface
        .common
        .nic_id
        .load(core::sync::atomic::Ordering::Relaxed) as u32;
    // Find which namespace owns this device — not necessarily current_netns()
    // (e.g., after IFLA_NET_NS_PID move).
    let own_ns = veth_iface
        .common
        .net_namespace
        .read()
        .as_ref()
        .and_then(|w| w.upgrade())
        .unwrap_or_else(crate::net::net_core::current_netns);

    if let Some(peer) = veth_iface.data.peer.lock().upgrade() {
        let peer_nic = peer
            .common
            .nic_id
            .load(core::sync::atomic::Ordering::Relaxed) as u32;
        let peer_ns = peer
            .common
            .net_namespace
            .read()
            .as_ref()
            .and_then(|w| w.upgrade())
            .unwrap_or_else(crate::net::net_core::current_netns);
        own_ns.remove_device(own_nic as usize);
        peer_ns.remove_device(peer_nic as usize);
        crate::net::config::NET_INTERFACE.remove_veth_stack(own_nic);
        crate::net::config::NET_INTERFACE.remove_veth_stack(peer_nic);
        log::info!(
            "[veth] deleted veth pair: {} (ifindex={}) <-> {} (ifindex={})",
            veth_iface.iface_name(),
            own_nic,
            peer.iface_name(),
            peer_nic
        );
    } else {
        own_ns.remove_device(own_nic as usize);
        crate::net::config::NET_INTERFACE.remove_veth_stack(own_nic);
        log::info!(
            "[veth] deleted veth (peer absent): {} (ifindex={})",
            veth_iface.iface_name(),
            own_nic
        );
    }
}

// ── veth_pair_new (backward-compat, will be removed in Wave 2) ────────

pub fn veth_pair_new(name1: &str, name2: &str) -> (u32, u32) {
    let ifindex1 = net_core::next_ifindex();
    let ifindex2 = net_core::next_ifindex();

    let iface1 = VethInterface::new(name1, ifindex1);
    let iface2 = VethInterface::new(name2, ifindex2);

    *iface1.data.peer.lock() = Arc::downgrade(&iface2);
    *iface2.data.peer.lock() = Arc::downgrade(&iface1);

    let flags = net_core::IFF_UP
        | net_core::IFF_BROADCAST
        | net_core::IFF_RUNNING
        | net_core::IFF_MULTICAST;
    iface1.set_flags(flags);
    iface2.set_flags(flags);

    net_core::add_device(iface1.clone());
    net_core::add_device(iface2.clone());

    let driver1 = VethDriver::new(iface1.data.clone());
    let driver2 = VethDriver::new(iface2.data.clone());
    crate::net::config::NET_INTERFACE.add_veth_stack(iface1.clone(), driver1);
    crate::net::config::NET_INTERFACE.add_veth_stack(iface2.clone(), driver2);

    (ifindex1, ifindex2)
}
