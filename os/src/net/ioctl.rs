use crate::mm::{UserPtr, UserPtrMut};
use crate::utils::error::SyscallErr;
use alloc::vec;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

pub const SIOCGIFCONF: u32 = 0x8912;
pub const SIOCGIFFLAGS: u32 = 0x8913;
pub const SIOCSIFFLAGS: u32 = 0x8914;
pub const SIOCGIFADDR: u32 = 0x8915;
pub const SIOCSIFADDR: u32 = 0x8916;
pub const SIOCGIFBRDADDR: u32 = 0x8919;
pub const SIOCGIFNETMASK: u32 = 0x891b;
pub const SIOCGIFMTU: u32 = 0x8921;
pub const SIOCSIFMTU: u32 = 0x8922;
pub const SIOCGIFHWADDR: u32 = 0x8927;
pub const SIOCGIFINDEX: u32 = 0x8933;
pub const SIOCGIFNAME: u32 = 0x8910;
pub const SIOCGIFTXQLEN: u32 = 0x8942;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ifreq { pub ifr_name: [u8; 16], pub ifr_data: [u8; 24] }
#[repr(C)]
#[derive(Copy, Clone)]
pub struct ifconf { pub ifc_len: i32, pub ifc_buf: usize }

impl ifreq {
    fn name_str(&self) -> &str {
        let len = self.ifr_name.iter().position(|&b| b == 0).unwrap_or(16);
        core::str::from_utf8(&self.ifr_name[..len]).unwrap_or("")
    }
    fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes(); let len = bytes.len().min(15);
        self.ifr_name[..len].copy_from_slice(&bytes[..len]); self.ifr_name[len] = 0;
    }
    fn ifr_ifindex(&self) -> u32 { u32::from_ne_bytes([self.ifr_data[0],self.ifr_data[1],self.ifr_data[2],self.ifr_data[3]]) }
    fn set_ifr_ifindex(&mut self, v: u32) { self.ifr_data[..4].copy_from_slice(&v.to_ne_bytes()); }
    fn ifr_flags(&self) -> u16 { u16::from_ne_bytes([self.ifr_data[0],self.ifr_data[1]]) }
    fn set_ifr_flags(&mut self, v: u16) { self.ifr_data[..2].copy_from_slice(&v.to_ne_bytes()); }
    fn ifr_mtu(&self) -> i32 { i32::from_ne_bytes([self.ifr_data[0],self.ifr_data[1],self.ifr_data[2],self.ifr_data[3]]) }
    fn set_ifr_mtu(&mut self, v: i32) { self.ifr_data[..4].copy_from_slice(&v.to_ne_bytes()); }
    fn ifr_addr(&self) -> Ipv4Address {
        Ipv4Address::new(self.ifr_data[4], self.ifr_data[5], self.ifr_data[6], self.ifr_data[7])
    }
    fn set_ifr_addr(&mut self, ip: Ipv4Address) {
        self.ifr_data[0..2].copy_from_slice(&(2u16).to_ne_bytes()); // AF_INET
        self.ifr_data[4..8].copy_from_slice(&ip.0);
    }
    fn set_ifr_hwaddr(&mut self, hw: &[u8; 6]) {
        self.ifr_data[0..2].copy_from_slice(&(1u16).to_ne_bytes()); // ARPHRD_ETHER
        self.ifr_data[2..8].copy_from_slice(hw);
    }
}

use crate::net::net_core;
use crate::task::current_task;

fn read_ifreq(arg: usize) -> Result<ifreq, SyscallErr> {
    let task = current_task().ok_or(SyscallErr::EINVAL)?;
    UserPtr::<ifreq>::from_addr(arg).read(task.get_user_token()).map_err(|_| SyscallErr::EFAULT)
}
fn write_ifreq(arg: usize, ifr: &ifreq) -> Result<(), SyscallErr> {
    let task = current_task().ok_or(SyscallErr::EINVAL)?;
    UserPtrMut::<ifreq>::from_addr(arg).write(task.get_user_token(), ifr).map_err(|_| SyscallErr::EFAULT)
}
fn find_dev(name: &str) -> Result<crate::net::net_core::DeviceEntry, SyscallErr> {
    crate::net::net_core::find_by_name(name).ok_or(SyscallErr::ENODEV)
}
fn siocgifconf(arg: usize) -> Result<usize, SyscallErr> {
    let task = current_task().ok_or(SyscallErr::EINVAL)?;
    let token = task.get_user_token();
    let mut conf: ifconf = UserPtr::<ifconf>::from_addr(arg).read(token).map_err(|_| SyscallErr::EFAULT)?;
    if conf.ifc_buf == 0 { conf.ifc_len = 0; UserPtrMut::<ifconf>::from_addr(arg).write(token, &conf).map_err(|_| SyscallErr::EFAULT)?; return Ok(0); }
    let max_bytes = conf.ifc_len as usize;
    let ns = net_core::current_netns();
    let list = ns.device_list.lock();
    let mut written = 0usize;
    for iface in list.values() {
        if written + 40 > max_bytes { break; }
        let mut ifr = ifreq { ifr_name: [0;16], ifr_data: [0;24] };
        ifr.set_name(&iface.iface_name());
        if let Some(cidr) = iface.ip_addrs().first() { if let IpAddress::Ipv4(addr) = cidr.address() { ifr.set_ifr_addr(addr); } }
        UserPtrMut::<ifreq>::from_addr(conf.ifc_buf + written).write(token, &ifr).map_err(|_| SyscallErr::EFAULT)?;
        written += 40;
    }
    conf.ifc_len = written as i32;
    UserPtrMut::<ifconf>::from_addr(arg).write(token, &conf).map_err(|_| SyscallErr::EFAULT)?;
    Ok(0)
}

fn siocgifindex(ifr: &mut ifreq) -> Result<usize, SyscallErr> { let d = find_dev(ifr.name_str())?; ifr.set_ifr_ifindex(d.ifindex); Ok(0) }
fn siocgifflags(ifr: &mut ifreq) -> Result<usize, SyscallErr> { let d = find_dev(ifr.name_str())?; ifr.set_ifr_flags(d.iface.flags() as u16); Ok(0) }
fn siocgifaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr> {
    let d = find_dev(ifr.name_str())?;
    match d.iface.ip_addrs().first().and_then(|c| match c.address() { IpAddress::Ipv4(a) => Some(a), _ => None }) {
        Some(a) => { ifr.set_ifr_addr(a); Ok(0) } None => Err(SyscallErr::EADDRNOTAVAIL),
    }
}
fn siocgifnetmask(ifr: &mut ifreq) -> Result<usize, SyscallErr> {
    let d = find_dev(ifr.name_str())?;
    let addrs = d.iface.ip_addrs();
    let prefix = addrs.first().map(|c| c.prefix_len()).unwrap_or(0);
    let mask: u32 = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
    let b = mask.to_be_bytes();
    ifr.set_ifr_addr(Ipv4Address::new(b[0], b[1], b[2], b[3]));
    Ok(0)
}
fn siocgifbrdaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr> {
    let d = find_dev(ifr.name_str())?;
    let addrs = d.iface.ip_addrs();
    match addrs.first().and_then(|c| match c.address() { IpAddress::Ipv4(a) => Some((a, c.prefix_len())), _ => None }) {
        Some((addr, prefix)) => {
            let b = addr.0;
            let ip = u32::from_be_bytes(b);
            let m = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
            let bcast = ip | !m; let b = bcast.to_be_bytes();
            ifr.set_ifr_addr(Ipv4Address::new(b[0], b[1], b[2], b[3]));
            Ok(0)
        } None => Err(SyscallErr::EADDRNOTAVAIL),
    }
}
fn siocgifname(ifr: &mut ifreq) -> Result<usize, SyscallErr> {
    let idx = ifr.ifr_ifindex();
    if idx == 0 { return Err(SyscallErr::ENXIO); }
    let ns = net_core::current_netns();
    let list = ns.device_list.lock();
    let d = list.get(&(idx as usize)).ok_or(SyscallErr::ENODEV)?;
    ifr.set_name(&d.iface_name());
    Ok(0)
}
fn siocgifmtu(ifr: &mut ifreq) -> Result<usize, SyscallErr> { let d = find_dev(ifr.name_str())?; ifr.set_ifr_mtu(d.iface.mtu() as i32); Ok(0) }
fn siocgifhwaddr(ifr: &mut ifreq) -> Result<usize, SyscallErr> { let d = find_dev(ifr.name_str())?; ifr.set_ifr_hwaddr(&d.iface.mac()); Ok(0) }
fn siocgiftxqlen(ifr: &mut ifreq) -> Result<usize, SyscallErr> {
    let _d = find_dev(ifr.name_str())?;
    ifr.set_ifr_mtu(1000); // reuse mtu field for qlen — same layout
    Ok(0)
}

pub fn siocgif_dispatch(cmd: u32, arg: usize) -> Result<usize, SyscallErr> {
    match cmd {
        SIOCGIFCONF => siocgifconf(arg),
        cmd if cmd == SIOCGIFINDEX || cmd == SIOCGIFNAME || cmd == SIOCGIFFLAGS || cmd == SIOCGIFADDR || cmd == SIOCGIFNETMASK || cmd == SIOCGIFBRDADDR || cmd == SIOCGIFMTU || cmd == SIOCGIFHWADDR || cmd == SIOCGIFTXQLEN => {
            let mut ifr = read_ifreq(arg)?;
            let r = match cmd {
                SIOCGIFINDEX => siocgifindex(&mut ifr),
                SIOCGIFNAME => siocgifname(&mut ifr),
                SIOCGIFFLAGS => siocgifflags(&mut ifr),
                SIOCGIFADDR => siocgifaddr(&mut ifr),
                SIOCGIFNETMASK => siocgifnetmask(&mut ifr),
                SIOCGIFBRDADDR => siocgifbrdaddr(&mut ifr),
                SIOCGIFMTU => siocgifmtu(&mut ifr),
                SIOCGIFHWADDR => siocgifhwaddr(&mut ifr),
                SIOCGIFTXQLEN => siocgiftxqlen(&mut ifr),
                _ => Err(SyscallErr::EOPNOTSUPP),
            };
            if r.is_ok() { write_ifreq(arg, &ifr)?; }
            r
        }
        SIOCSIFFLAGS => {
            let ifr = read_ifreq(arg)?;
            let name = ifr.name_str();
            let new_low16 = ifr.ifr_flags() as u32 & 0xFFFF;
            let ifindex = {
                let ns = net_core::current_netns();
                let list = ns.device_list.lock();
                let iface = list.values().find(|iface| iface.iface_name() == name)
                    .ok_or(SyscallErr::ENODEV)?;
                let combined = (iface.flags() & 0xFFFF0000) | new_low16;
                iface.set_flags(combined);
                iface.nic_id() as u32
            };
            // Drop device_list lock before accessing NET_INTERFACE (lock ordering)
            crate::net::config::NET_INTERFACE.inner_handler(|inner| {
                let _ = inner.stack_mut(ifindex);
                // smoltcp 0.10 has no native up/down state; flags already synced to Iface
            });
            Ok(0)
        }
        SIOCSIFADDR => {
            let ifr = read_ifreq(arg)?;
            let name = ifr.name_str();
            let new_addr = ifr.ifr_addr();
            let (ifindex, prefix) = {
                let ns = net_core::current_netns();
                let list = ns.device_list.lock();
                let iface = list.values().find(|iface| iface.iface_name() == name)
                    .ok_or(SyscallErr::ENODEV)?;
                let prefix = iface.ip_addrs().first().map(|c| c.prefix_len()).unwrap_or(24);
                for old in iface.ip_addrs() {
                    iface.del_ip_addr(old);
                }
                iface.add_ip_addr(IpCidr::new(IpAddress::Ipv4(new_addr), prefix));
                (iface.nic_id() as u32, prefix)
            };
            // Sync IP addresses to smoltcp Interface
            let new_cidr = IpCidr::new(IpAddress::Ipv4(new_addr), prefix);
            crate::net::config::NET_INTERFACE.inner_handler(|inner| {
                if let Some(stack) = inner.stack_mut(ifindex) {
                    stack.iface.update_ip_addrs(|addrs| {
                        addrs.clear();
                        let _ = addrs.push(new_cidr);
                    });
                }
            });
            Ok(0)
        }
        SIOCSIFMTU => {
            let ifr = read_ifreq(arg)?;
            let name = ifr.name_str();
            let new_mtu = ifr.ifr_mtu();
            let ifindex = {
                let ns = net_core::current_netns();
                let list = ns.device_list.lock();
                let iface = list.values().find(|iface| iface.iface_name() == name)
                    .ok_or(SyscallErr::ENODEV)?;
                iface.set_mtu(new_mtu as usize);
                iface.nic_id() as u32
            };
            // Sync MTU to smoltcp Interface capabilities
            let mtu = new_mtu as usize;
            crate::net::config::NET_INTERFACE.inner_handler(|inner| {
                if let Some(stack) = inner.stack_mut(ifindex) {
                    stack.iface.set_mtu(mtu);
                }
            });
            Ok(0)
        }
        _ => Err(SyscallErr::EOPNOTSUPP),
    }
}
