//! RTM_NEWADDR / RTM_DELADDR handlers for the netlink route family.
//!
//! Supports IPv4 (AF_INET) only.  Parses [`CIfaddrMsg`]-equivalent wire format
//! (8-byte header + RTA attributes), validates the interface, and adds or
//! removes the address via [`Iface::add_ip_addr`] / [`Iface::del_ip_addr`].

use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use crate::net::iface::Iface;
use crate::utils::error::SyscallErr;

use super::super::NetlinkSocket;
use super::super::netlink::{build_nlmsg_error, IFA_ADDRESS, IFA_LOCAL, NLM_F_EXCL, NLM_F_REPLACE};

/// Parse an IPv4 address from the RTA attributes following the `ifaddrmsg` header.
fn parse_ifa_addr(payload: &[u8], mut offset: usize) -> Option<Ipv4Address> {
    let mut ip_addr: Option<Ipv4Address> = None;
    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let data_start = offset + 4;
        let data_len = rta_len - 4;

        match rta_type {
            IFA_ADDRESS | IFA_LOCAL if data_len >= 4 => {
                if ip_addr.is_none() {
                    ip_addr = Some(Ipv4Address([
                        payload[data_start],
                        payload[data_start + 1],
                        payload[data_start + 2],
                        payload[data_start + 3],
                    ]));
                }
            }
            _ => {}
        }
        offset += (rta_len + 3) & !3;
    }
    ip_addr
}

/// Look up an interface by its numeric index.
fn find_iface_by_index(index: i32) -> Option<alloc::sync::Arc<dyn Iface>> {
    let ns = crate::net::net_core::current_netns();
    let list = ns.device_list.lock();
    for iface in list.values() {
        if iface.nic_id() as i32 == index {
            return Some(iface.clone());
        }
    }
    None
}

/// Push an `NLMSG_ERROR` ACK onto the socket's receive queue.
/// Returns `Err(ENOBUFS)` if the queue is full.
fn send_ack(sock: &NetlinkSocket, seq: u32, pid: u32, errno: i32, orig: &[u8; 16]) -> Result<(), SyscallErr> {
    if !sock.push_recv(build_nlmsg_error(errno, seq, pid, orig)) {
        return Err(SyscallErr::ENOBUFS);
    }
    Ok(())
}

/// Decodes the 8-byte ifaddrmsg wire header.
struct IfaddrMsg {
    family: u8,
    prefixlen: u8,
    flags: u8,
    scope: u8,
    index: i32,
}

fn parse_ifaddrmsg(payload: &[u8]) -> Option<IfaddrMsg> {
    if payload.len() < 8 {
        return None;
    }
    Some(IfaddrMsg {
        family: payload[0],
        prefixlen: payload[1],
        flags: payload[2],
        scope: payload[3],
        index: i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]),
    })
}

/// Handle `RTM_NEWADDR` — add or replace an IP address on an interface.
///
/// Behaviour (matching Linux semantics):
///
/// | `NLM_F_EXCL` | `NLM_F_REPLACE` | Address exists | Result              |
/// |-------------|-----------------|----------------|---------------------|
/// | set         | —               | yes            | `EEXIST`            |
/// | —           | set             | yes            | replace (del + add) |
/// | —           | —               | yes            | `EEXIST`            |
/// | —           | —               | no             | add                 |
///
/// Errors: `EINVAL`, `EAFNOSUPPORT`, `ENODEV`, `EEXIST`.
pub fn handle_newaddr(
    seq: u32,
    pid: u32,
    nl_flags: u16,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, SyscallErr> {
    let payload = buf.get(16..).ok_or(SyscallErr::EINVAL)?;
    let msg = parse_ifaddrmsg(payload).ok_or(SyscallErr::EINVAL)?;

    if msg.family != 2 {
        return Err(SyscallErr::EAFNOSUPPORT);
    }

    let addr = parse_ifa_addr(payload, 8).ok_or(SyscallErr::EINVAL)?;
    let cidr = IpCidr::new(IpAddress::Ipv4(addr), msg.prefixlen);

    let iface = find_iface_by_index(msg.index).ok_or(SyscallErr::ENODEV)?;

    let exists = iface.ip_addrs().iter().any(|c| *c == cidr);

    if exists {
        if nl_flags & NLM_F_REPLACE != 0 {
            iface.del_ip_addr(cidr);
            iface.add_ip_addr(cidr);
        } else if nl_flags & NLM_F_EXCL != 0 {
            return Err(SyscallErr::EEXIST);
        } else {
            return Err(SyscallErr::EEXIST);
        }
    } else {
        iface.add_ip_addr(cidr);
    }

    sync_addr_to_smoltcp(msg.index as u32, cidr);

    {
        let ns = crate::net::net_core::current_netns();
        let subnet = IpCidr::new(IpAddress::Ipv4(addr), msg.prefixlen);
        let mut router = ns.router.lock();
        router.add_route(
            subnet,
            None,
            msg.index as u32,
            0,
            crate::net::routing::RouteType::Default,
        );
    }

    let mut orig = [0u8; 16];
    orig.copy_from_slice(&buf[..16]);
    send_ack(sock, seq, pid, 0, &orig)?;
    Ok(0)
}

/// Handle `RTM_DELADDR` — remove an IP address from an interface.
///
/// Errors: `EINVAL`, `EAFNOSUPPORT`, `ENODEV`.
pub fn handle_deladdr(
    seq: u32,
    pid: u32,
    _nl_flags: u16,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, SyscallErr> {
    let payload = buf.get(16..).ok_or(SyscallErr::EINVAL)?;
    let msg = parse_ifaddrmsg(payload).ok_or(SyscallErr::EINVAL)?;

    if msg.family != 2 {
        return Err(SyscallErr::EAFNOSUPPORT);
    }

    let addr = parse_ifa_addr(payload, 8).ok_or(SyscallErr::EINVAL)?;
    let cidr = IpCidr::new(IpAddress::Ipv4(addr), msg.prefixlen);

    let iface = find_iface_by_index(msg.index).ok_or(SyscallErr::ENODEV)?;
    iface.del_ip_addr(cidr);

    unsync_addr_from_smoltcp(msg.index as u32, cidr);

    {
        let ns = crate::net::net_core::current_netns();
        let mut router = ns.router.lock();
        router.table.remove(&cidr);
    }

    let mut orig = [0u8; 16];
    orig.copy_from_slice(&buf[..16]);
    send_ack(sock, seq, pid, 0, &orig)?;
    Ok(0)
}

fn sync_addr_to_smoltcp(ifindex: u32, cidr: IpCidr) {
    crate::net::config::NET_INTERFACE.add_ip_to_stack(ifindex, cidr);
}

fn unsync_addr_from_smoltcp(ifindex: u32, cidr: IpCidr) {
    crate::net::config::NET_INTERFACE.remove_ip_from_stack(ifindex, cidr);
}
