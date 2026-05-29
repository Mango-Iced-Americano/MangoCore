use super::NetlinkSocket;
use super::netlink::*;
use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, Ipv4Address};

fn parse_nlmsg(buf: &[u8]) -> Option<(u16, u16, u32, u32)> {
    if buf.len() < 16 { return None; }
    let t = u16::from_ne_bytes([buf[4],buf[5]]);
    let f = u16::from_ne_bytes([buf[6],buf[7]]);
    let s = u32::from_ne_bytes([buf[8],buf[9],buf[10],buf[11]]);
    let p = u32::from_ne_bytes([buf[12],buf[13],buf[14],buf[15]]);
    Some((t, f, s, p))
}

pub fn handle_netlink_msg(buf: &[u8], sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let (msg_type, flags, seq, pid) = match parse_nlmsg(buf) {
        Some(v) => v, None => return Err(crate::utils::error::SyscallErr::EINVAL),
    };
    if flags & NLM_F_REQUEST == 0 { return Ok(0); }
    if flags & NLM_F_DUMP != NLM_F_DUMP {
        let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
        sock.recv_queue.lock().push_back(build_nlmsg_error(95, seq, pid, &orig));
        return Ok(0);
    }
    match msg_type {
        RTM_GETLINK => handle_getlink(seq, pid, sock),
        RTM_GETADDR => handle_getaddr(seq, pid, sock),
        RTM_GETROUTE => handle_getroute(seq, pid, sock),
        _ => {
            let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
            sock.recv_queue.lock().push_back(build_nlmsg_error(95, seq, pid, &orig));
            Ok(0)
        }
    }
}

fn handle_getlink(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let mut q = sock.recv_queue.lock();
    let ifaces = crate::net::net_core::IFACES.lock();
    for dev in ifaces.iter() {
        let mut payload = Vec::new();
        payload.push(0); payload.push(0);
        let ift = if dev.ifindex == 1 { ARPHRD_LOOPBACK } else { ARPHRD_ETHER };
        payload.extend_from_slice(&ift.to_ne_bytes());
        payload.extend_from_slice(&dev.ifindex.to_ne_bytes());
        payload.extend_from_slice(&dev.flags.to_ne_bytes());
        payload.extend_from_slice(&0u32.to_ne_bytes());
        let mut n = Vec::new(); n.extend_from_slice(dev.name.as_bytes()); n.push(0);
        payload.extend(&rta_data(IFLA_IFNAME, &n));
        payload.extend(&rta_data(IFLA_MTU, &dev.mtu.to_ne_bytes()));
        if dev.ifindex == 2 { payload.extend(&rta_data(IFLA_ADDRESS, &dev.hwaddr)); }
        q.push_back(build_nlmsg(RTM_NEWLINK, NLM_F_MULTI, seq, pid, &payload));
    }
    q.push_back(build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[]));
    Ok(0)
}

fn handle_getaddr(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let mut q = sock.recv_queue.lock();
    let ifaces = crate::net::net_core::IFACES.lock();
    for dev in ifaces.iter() {
        for cidr in &dev.ip_addrs {
            if let IpAddress::Ipv4(addr) = cidr.address() {
                let mut payload = Vec::new();
                payload.push(2); payload.push(0);
                payload.push(cidr.prefix_len()); payload.push(0);
                payload.push(0);
                payload.extend_from_slice(&dev.ifindex.to_ne_bytes());
                let mut attrs = Vec::new();
                attrs.extend(&rta_data(IFA_ADDRESS, &addr.0));
                attrs.extend(&rta_data(IFA_LOCAL, &addr.0));
                let mut label = Vec::new(); label.extend_from_slice(dev.name.as_bytes()); label.push(0);
                attrs.extend(&rta_data(IFA_LABEL, &label));
                payload.extend(&attrs);
                q.push_back(build_nlmsg(RTM_NEWADDR, NLM_F_MULTI, seq, pid, &payload));
            }
        }
    }
    q.push_back(build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[]));
    Ok(0)
}

fn handle_getroute(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let mut q = sock.recv_queue.lock();
    let router = crate::net::routing::Router::init_default();
    for entry in &router.table.entries {
        let mut payload = Vec::new();
        payload.push(2); payload.push(0);
        payload.push(entry.destination.prefix_len()); payload.push(0);
        payload.push(0); payload.push(2); payload.push(3); payload.push(0);
        let rt = match entry.route_type { crate::net::routing::RouteType::Default => 3u8, _ => 1u8 };
        payload.push(rt); payload.push(0);
        let mut attrs = Vec::new();
        if entry.destination.prefix_len() > 0 {
            if let IpAddress::Ipv4(a) = entry.destination.address() { attrs.extend(&rta_data(RTA_DST, &a.0)); }
        }
        if let Some(nh) = entry.next_hop {
            if let IpAddress::Ipv4(a) = nh { attrs.extend(&rta_data(RTA_GATEWAY, &a.0)); }
        }
        attrs.extend(&rta_data(RTA_OIF, &entry.ifindex.to_ne_bytes()));
        payload.extend(&attrs);
        q.push_back(build_nlmsg(RTM_NEWROUTE, NLM_F_MULTI, seq, pid, &payload));
    }
    q.push_back(build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[]));
    Ok(0)
}
