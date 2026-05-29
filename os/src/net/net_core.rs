use super::Mutex;
use alloc::vec;
use alloc::vec::Vec;
use crate::drivers::NET_DEVICE;
use lazy_static::*;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

pub const IFF_UP: u32 = 0x1;
pub const IFF_BROADCAST: u32 = 0x2;
pub const IFF_LOOPBACK: u32 = 0x8;
pub const IFF_RUNNING: u32 = 0x40;
pub const IFF_NOARP: u32 = 0x80;
pub const IFF_MULTICAST: u32 = 0x1000;

pub const IF_OPER_UP: u8 = 6;

#[derive(Clone, Debug)]
pub struct DeviceEntry {
    pub ifindex: u32,
    pub name: &'static str,
    pub flags: u32,
    pub mtu: u32,
    pub hwaddr: [u8; 6],
    pub ip_addrs: Vec<IpCidr>,
    pub operstate: u8,
}

lazy_static! {
    pub static ref IFACES: Mutex<Vec<DeviceEntry>> = Mutex::new(Vec::new());
}

/// 内部注册（不锁 IFACES，供持有锁的调用者使用）
fn _register_device(
    ifaces: &mut Vec<DeviceEntry>,
    name: &'static str,
    flags: u32,
    mtu: u32,
    hwaddr: [u8; 6],
    ip_addrs: Vec<IpCidr>,
) -> u32 {
    let ifindex = ifaces.len() as u32 + 1;
    ifaces.push(DeviceEntry {
        ifindex,
        name,
        flags,
        mtu,
        hwaddr,
        ip_addrs,
        operstate: IF_OPER_UP,
    });
    ifindex
}

pub fn register_device(
    name: &'static str,
    flags: u32,
    mtu: u32,
    hwaddr: [u8; 6],
    ip_addrs: Vec<IpCidr>,
) -> u32 {
    let mut ifaces = IFACES.lock();
    _register_device(&mut ifaces, name, flags, mtu, hwaddr, ip_addrs)
}

pub fn find_by_name(name: &str) -> Option<DeviceEntry> {
    let ifaces = IFACES.lock();
    ifaces.iter().find(|d| d.name == name).cloned()
}

pub fn find_by_index(idx: u32) -> Option<DeviceEntry> {
    let ifaces = IFACES.lock();
    ifaces.iter().find(|d| d.ifindex == idx).cloned()
}

/// 初始化网络设备表（幂等）
pub fn init() {
    let mut ifaces = IFACES.lock();
    if ifaces.len() > 0 {
        return;
    }

    let lo_ip = IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8);
    _register_device(
        &mut ifaces,
        "lo",
        IFF_UP | IFF_LOOPBACK | IFF_RUNNING,
        65536,
        [0u8; 6],
        vec![lo_ip],
    );
    log::info!("[net_core] registered lo (ifindex=1)");

    let net_guard = NET_DEVICE.lock();
    if let Some(dev) = net_guard.as_ref() {
        let mac = dev.mac_address();
        drop(net_guard);

        let eth_ip = IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24);
        _register_device(
            &mut ifaces,
            "eth0",
            IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST,
            1500,
            mac,
            vec![eth_ip],
        );
        log::info!("[net_core] registered eth0 (ifindex=2)");
    } else {
        drop(net_guard);
    }
}

/// Return the default network interface: eth0 if registered, otherwise lo.
pub fn default_iface() -> Option<DeviceEntry> {
    let ifaces = IFACES.lock();
    // eth0 is the default (typically ifindex=2). If not found, fall back to lo.
    let found = ifaces.iter().find(|d| d.name == "eth0").cloned();
    if found.is_some() {
        found
    } else {
        ifaces.iter().find(|d| d.name == "lo").cloned()
    }
}

/// Return the loopback interface "lo".
pub fn loopback_iface() -> Option<DeviceEntry> {
    let ifaces = IFACES.lock();
    ifaces.iter().find(|d| d.name == "lo").cloned()
}

/// Return the default gateway address (10.0.2.2) if eth0 is registered, otherwise None.
pub fn default_gateway() -> Option<Ipv4Address> {
    let ifaces = IFACES.lock();
    if ifaces.iter().any(|d| d.name == "eth0") {
        Some(Ipv4Address::new(10, 0, 2, 2))
    } else {
        None
    }
}

/// Return the local port range for ephemeral ports (32768–60999 on Linux).
pub fn local_port_range() -> (u16, u16) {
    (32_768, 60_999)
}

/// Return the IP address of the interface identified by `ifindex`.
pub fn iface_ip(ifindex: u32) -> Option<IpAddress> {
    let ifaces = IFACES.lock();
    ifaces
        .iter()
        .find(|d| d.ifindex == ifindex)
        .and_then(|d| d.ip_addrs.first().map(|cidr| cidr.address()))
}
