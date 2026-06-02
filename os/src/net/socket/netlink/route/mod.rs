pub mod link;
pub mod addr;
pub mod route;

use super::NetlinkSocket;
use super::netlink::{
    ARPHRD_ETHER, ARPHRD_LOOPBACK,
    IFLA_ADDRESS, IFLA_IFNAME, IFLA_MTU,
    IFA_ADDRESS, IFA_LOCAL, IFA_LABEL,
    NLMSG_DONE, NLMSG_ERROR, NLM_F_DUMP, NLM_F_MULTI, NLM_F_REQUEST,
    RTA_DST, RTA_GATEWAY, RTA_OIF,
    RTM_DELADDR, RTM_DELLINK, RTM_DELROUTE, RTM_GETADDR, RTM_GETLINK, RTM_GETROUTE,
    RTM_NEWADDR, RTM_NEWLINK, RTM_NEWROUTE,
    RTM_SETLINK,
};
use super::segment::{
    CMsgSegHdr, DoneSegment, DoneSegmentBody,
    ErrorSegment, ErrorSegmentBody, NoAttr, RouteNlSegment, SegmentCommon,
};
use alloc::vec::Vec;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use crate::net::iface::DeviceKind;

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
        // Write operations (non-dump requests)
        match msg_type {
            RTM_NEWADDR => return addr::handle_newaddr(seq, pid, flags, buf, sock),
            RTM_DELADDR => return addr::handle_deladdr(seq, pid, flags, buf, sock),
            RTM_NEWLINK => return link::handle_newlink(seq, pid, buf, flags, sock),
            RTM_DELLINK => return link::handle_dellink(seq, pid, buf, sock),
            RTM_SETLINK => return link::handle_setlink(seq, pid, buf, sock),
            RTM_NEWROUTE => return route::handle_newroute(seq, pid, buf, flags, sock),
            RTM_DELROUTE => return route::handle_delroute(seq, pid, buf, sock),
            _ => {}
        }
        let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
        if !sock.push_recv(super::netlink::build_nlmsg_error(95, seq, pid, &orig)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
        return Ok(0);
    }
    match msg_type {
        RTM_GETLINK => handle_getlink(seq, pid, sock),
        RTM_GETADDR => handle_getaddr(seq, pid, sock),
        RTM_GETROUTE => handle_getroute(seq, pid, sock),
        _ => {
            let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
            if !sock.push_recv(super::netlink::build_nlmsg_error(95, seq, pid, &orig)) {
                return Err(crate::utils::error::SyscallErr::ENOBUFS);
            }
            Ok(0)
        }
    }
}

fn handle_getlink(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let ns = crate::net::net_core::current_netns();
    let list = ns.device_list.lock();
    for (nic_id, iface) in list.iter() {
        let nic_id = *nic_id as u32;
        let mut payload = Vec::new();
        payload.push(0); payload.push(0);
        let ift = if nic_id == 1 { ARPHRD_LOOPBACK } else { ARPHRD_ETHER };
        payload.extend_from_slice(&ift.to_ne_bytes());
        payload.extend_from_slice(&nic_id.to_ne_bytes());
        payload.extend_from_slice(&iface.flags().to_ne_bytes());
        payload.extend_from_slice(&0u32.to_ne_bytes());
        let mut n = Vec::new(); n.extend_from_slice(iface.iface_name().as_bytes()); n.push(0);
        payload.extend(&super::netlink::rta_data(IFLA_IFNAME, &n));
        payload.extend(&super::netlink::rta_data(IFLA_MTU, &(iface.mtu() as u32).to_ne_bytes()));
        if iface.kind() != DeviceKind::Loopback { payload.extend(&super::netlink::rta_data(IFLA_ADDRESS, &iface.mac())); }
        if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWLINK, NLM_F_MULTI, seq, pid, &payload)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
    }
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[])) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

fn handle_getaddr(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let ns = crate::net::net_core::current_netns();
    let list = ns.device_list.lock();
    for (nic_id, iface) in list.iter() {
        let nic_id = *nic_id as u32;
        for cidr in &iface.ip_addrs() {
            if let IpAddress::Ipv4(addr) = cidr.address() {
                let mut payload = Vec::new();
                // ifaddrmsg: family(1) + prefixlen(1) + flags(1) + scope(1) + ifa_index(4) = 8 bytes
                payload.push(2); // AF_INET
                payload.push(cidr.prefix_len());
                payload.push(0); // flags
                payload.push(0); // scope
                payload.extend_from_slice(&nic_id.to_ne_bytes());
                let mut attrs = Vec::new();
                attrs.extend(&super::netlink::rta_data(IFA_ADDRESS, &addr.0));
                attrs.extend(&super::netlink::rta_data(IFA_LOCAL, &addr.0));
                let mut label = Vec::new(); label.extend_from_slice(iface.iface_name().as_bytes()); label.push(0);
                attrs.extend(&super::netlink::rta_data(IFA_LABEL, &label));
                payload.extend(&attrs);
                if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWADDR, NLM_F_MULTI, seq, pid, &payload)) {
                    return Err(crate::utils::error::SyscallErr::ENOBUFS);
                }
            }
        }
    }
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[])) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

fn handle_getroute(seq: u32, pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let entries = crate::net::net_core::current_netns().router.lock().table.entries.clone();
    for entry in &entries {
        // rtmsg: family(1) + dst_len(1) + src_len(1) + tos(1) + table(1) +
        //        protocol(1) + scope(1) + type_(1) + flags(4) = 12 bytes
        let mut payload = Vec::new();
        let rt = match entry.route_type { crate::net::routing::RouteType::Default => 3u8, _ => 1u8 };
        payload.push(2); // AF_INET
        payload.push(entry.destination.prefix_len()); // dst_len
        payload.push(0); // src_len
        payload.push(0); // tos
        payload.push(0); // table
        payload.push(2); // protocol (RTPROT_BOOT)
        payload.push(3); // scope (RT_SCOPE_UNIVERSE)
        payload.push(rt); // type_ (RTN_UNICAST=1, RTN_UNICAST=default=3)
        payload.extend_from_slice(&0u32.to_ne_bytes()); // flags
        let mut attrs = Vec::new();
        if entry.destination.prefix_len() > 0 {
            if let IpAddress::Ipv4(a) = entry.destination.address() { attrs.extend(&super::netlink::rta_data(RTA_DST, &a.0)); }
        }
        if let Some(nh) = entry.next_hop {
            if let IpAddress::Ipv4(a) = nh { attrs.extend(&super::netlink::rta_data(RTA_GATEWAY, &a.0)); }
        }
        attrs.extend(&super::netlink::rta_data(RTA_OIF, &entry.ifindex.to_ne_bytes()));
        payload.extend(&attrs);
        if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWROUTE, NLM_F_MULTI, seq, pid, &payload)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
    }
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, NLM_F_MULTI, seq, pid, &[])) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

/// Align length to 4-byte boundary (NLMSG_ALIGNTO = 4).
pub fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

/// Align RTA attribute length to 4-byte boundary (RTA_ALIGNTO = 4).
pub fn rta_align(len: usize) -> usize {
    (len + 3) & !3
}

/// Build an `NLMSG_ERROR` segment with the given error code.
///
/// `errno` is negated per Linux netlink convention (positive value
/// stored as negative in the wire format).
pub fn build_nlmsg_error(seg_hdr: &CMsgSegHdr, errno: i32) -> ErrorSegment {
    let body = ErrorSegmentBody {
        error_code: -errno,
        request_header: *seg_hdr,
    };
    // Body layout: error_code(4) + request_header(16) = 20 bytes
    let body_len = 4 + core::mem::size_of::<CMsgSegHdr>();
    let total_len = core::mem::size_of::<CMsgSegHdr>() + nlmsg_align(body_len);
    SegmentCommon {
        header: CMsgSegHdr {
            len: total_len as u32,
            type_: NLMSG_ERROR,
            flags: 0,
            seq: seg_hdr.seq,
            pid: seg_hdr.pid,
        },
        body,
        attrs: Vec::new(),
    }
}

/// Build an `NLMSG_DONE` segment (end-of-multipart marker).
pub fn build_nlmsg_done(seg_hdr: &CMsgSegHdr) -> DoneSegment {
    let body = DoneSegmentBody { error_code: 0 };
    // Body layout: error_code(4) = 4 bytes
    let body_len = 4;
    let total_len = core::mem::size_of::<CMsgSegHdr>() + nlmsg_align(body_len);
    SegmentCommon {
        header: CMsgSegHdr {
            len: total_len as u32,
            type_: NLMSG_DONE,
            flags: 0,
            seq: seg_hdr.seq,
            pid: seg_hdr.pid,
        },
        body,
        attrs: Vec::new(),
    }
}

/// Build a success ACK segment (`NLMSG_ERROR` with `error_code = 0`).
pub fn build_nlmsg_ack(seg_hdr: &CMsgSegHdr) -> ErrorSegment {
    build_nlmsg_error(seg_hdr, 0)
}

/// Finalise a response segment list:
///
/// - If `was_dump` is true, append an `NLMSG_DONE` segment.
/// - Set `NLM_F_MULTI` on every segment in the list.
pub fn finish_response(segments: &mut Vec<RouteNlSegment>, was_dump: bool) {
    if was_dump && !segments.is_empty() {
        let (last_seq, last_pid) = {
            let last = segments.last().unwrap();
            match last {
                RouteNlSegment::NewLink(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::DelLink(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::SetLink(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::GetLink(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::NewAddr(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::DelAddr(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::GetAddr(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::NewRoute(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::DelRoute(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::GetRoute(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::Error(s) => (s.header.seq, s.header.pid),
                RouteNlSegment::Done(s) => (s.header.seq, s.header.pid),
            }
        };
        let done_hdr = CMsgSegHdr {
            len: 0,
            type_: 0,
            flags: 0,
            seq: last_seq,
            pid: last_pid,
        };
        segments.push(RouteNlSegment::Done(build_nlmsg_done(&done_hdr)));
    }

    for seg in segments.iter_mut() {
        match seg {
            RouteNlSegment::NewLink(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::DelLink(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::SetLink(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::GetLink(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::NewAddr(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::DelAddr(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::GetAddr(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::NewRoute(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::DelRoute(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::GetRoute(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::Error(s) => s.header.flags |= NLM_F_MULTI,
            RouteNlSegment::Done(s) => s.header.flags |= NLM_F_MULTI,
        }
    }
}
