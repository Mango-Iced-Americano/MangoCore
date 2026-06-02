//! Network namespace with per-ns device list and routing table.
//!
//! Each [`NetNamespace`] owns its own set of network devices and routing table,
//! providing network stack isolation between namespaces.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use crate::net::iface::{DeviceKind, Iface};
use crate::net::net_core::{NetDeviceEntry, IF_OPER_UP, IFF_LOOPBACK, IFF_RUNNING, IFF_UP};
use crate::net::routing::Router;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use smoltcp::wire::{IpAddress, IpCidr};
use spin::Mutex;

/// Network namespace: owns a device set and an IPv4 routing table.
pub struct NetNamespace {
    pub id: u64,
    pub device_list: Mutex<BTreeMap<usize, Arc<dyn Iface>>>,
    pub router: Mutex<Router>,
}

impl core::fmt::Debug for NetNamespace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NetNamespace")
            .field("id", &self.id)
            .field("device_list", &self.device_list.lock().len())
            .finish()
    }
}

lazy_static! {
    /// Boot-time init namespace (id=0). Loopback and virtio-eth are registered
    /// here during `net_core::init()`.
    pub static ref INIT_NET_NAMESPACE: Arc<NetNamespace> = Arc::new(NetNamespace {
        id: 0,
        device_list: Mutex::new(BTreeMap::new()),
        router: Mutex::new(Router::new()),
    });
}

static NEXT_NS_ID: AtomicU64 = AtomicU64::new(1);

impl NetNamespace {
    pub fn new() -> Arc<Self> {
        let id = NEXT_NS_ID.fetch_add(1, Ordering::Relaxed);
        Arc::new(Self {
            id,
            device_list: Mutex::new(BTreeMap::new()),
            router: Mutex::new(Router::new()),
        })
    }

    /// Create a new isolated network namespace with only loopback registered.
    /// Used by CLONE_NEWNET and unshare(CLONE_NEWNET).
    pub fn new_isolated() -> Arc<Self> {
        let ns = Self::new();

        // Register loopback (same as net_core::init())
        let lo = Arc::new(NetDeviceEntry::new(
            String::from("lo"),
            DeviceKind::Loopback,
            [0u8; 6],
            65536,
            IFF_UP | IFF_LOOPBACK | IFF_RUNNING,
            vec![IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)],
            None,
            IF_OPER_UP as u32,
        ));
        lo.set_nic_id(1);
        ns.add_device(lo);

        ns
    }

    pub fn add_device(&self, iface: Arc<dyn Iface>) {
        let nic_id = iface.nic_id();
        self.device_list.lock().insert(nic_id, iface);
    }

    pub fn remove_device(&self, nic_id: usize) {
        self.device_list.lock().remove(&nic_id);
    }

    pub fn device_by_index(&self, ifindex: usize) -> Option<Arc<dyn Iface>> {
        self.device_list.lock().get(&ifindex).cloned()
    }

    pub fn device_by_name(&self, name: &str) -> Option<Arc<dyn Iface>> {
        self.device_list
            .lock()
            .values()
            .find(|iface| iface.iface_name() == name)
            .cloned()
    }
}

lazy_static! {
    static ref NS_BY_PID: spin::Mutex<BTreeMap<usize, Weak<NetNamespace>>> =
        spin::Mutex::new(BTreeMap::new());
}

pub fn register_ns_for_pid(pid: usize, ns: &Arc<NetNamespace>) {
    NS_BY_PID.lock().insert(pid, Arc::downgrade(ns));
}

pub fn find_ns_by_pid(pid: usize) -> Option<Arc<NetNamespace>> {
    NS_BY_PID.lock().get(&pid).and_then(|w| w.upgrade())
}
