pub mod link;
pub mod addr;
pub mod route;

use super::NetlinkSocket;
use super::netlink::{
    ARPHRD_ETHER, ARPHRD_LOOPBACK,
    IFLA_ADDRESS, IFLA_IFNAME, IFLA_MTU,
    IFA_ADDRESS, IFA_LOCAL, IFA_LABEL,
    NDA_DST, NDA_LLADDR,
    NLMSG_DONE, NLMSG_ERROR, NLM_F_DUMP, NLM_F_MULTI, NLM_F_REQUEST, NLM_F_ROOT,
    RTA_DST, RTA_GATEWAY, RTA_OIF,
    RTM_DELADDR, RTM_DELLINK, RTM_DELROUTE, RTM_GETADDR, RTM_GETLINK, RTM_GETROUTE,
    RTM_GETNEIGH, RTM_NEWADDR, RTM_NEWLINK, RTM_NEWROUTE,
    RTM_DELNEIGH, RTM_NEWNEIGH, RTM_SETLINK,
};
use super::segment::{
    CMsgSegHdr, DoneSegment, DoneSegmentBody,
    ErrorSegment, ErrorSegmentBody, NoAttr, RouteNlSegment, SegmentCommon,
};
use alloc::vec::Vec;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address, Ipv6Address};
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
    let (msg_type, flags, seq, _req_pid) = match parse_nlmsg(buf) {
        Some(v) => v,
        None => {
            log::error!("[netlink] handle_netlink_msg: failed to parse nlmsghdr (len={})", buf.len());
            return Ok(0);
        }
    };
    log::warn!("[netlink] handle msg type={} flags={:#x} seq={} pid={}", msg_type, flags, seq, _req_pid);

    if flags & NLM_F_REQUEST == 0 {
        log::error!("[netlink] ignoring non-request message (flags={:#x})", flags);
        return Ok(0);
    }

    let pid = sock.local_portid();

    let is_get = matches!(msg_type, RTM_GETLINK | RTM_GETADDR | RTM_GETROUTE | RTM_GETNEIGH);
    let is_dump = (flags & (NLM_F_DUMP | NLM_F_ROOT)) != 0;
    if !is_get || !is_dump {
        // Single-object handler (NEW/DEL/SET, or GET without DUMP)
        let result = match msg_type {
            RTM_NEWADDR => addr::handle_newaddr(seq, pid, flags, buf, sock),
            RTM_DELADDR => addr::handle_deladdr(seq, pid, flags, buf, sock),
            RTM_NEWLINK => link::handle_newlink(seq, pid, buf, flags, sock),
            RTM_DELLINK => link::handle_dellink(seq, pid, buf, sock),
            RTM_SETLINK => link::handle_setlink(seq, pid, buf, sock),
            RTM_NEWROUTE => route::handle_newroute(seq, pid, buf, flags, sock),
            RTM_DELROUTE => route::handle_delroute(seq, pid, buf, sock),
            RTM_DELNEIGH => handle_delneigh(seq, pid, buf, sock),
            RTM_NEWNEIGH => handle_newneigh(seq, pid, buf, sock),
            RTM_GETNEIGH => handle_getneigh_single(seq, pid, buf, sock),
            RTM_GETLINK => handle_getlink_single(seq, pid, buf, sock),
            _ => Err(crate::utils::error::SyscallErr::EOPNOTSUPP),
        };

        if let Err(e) = result {
            let errno = e as i32;
            log::warn!("[netlink] handler for msg_type={} failed: {:?} (errno={})", msg_type, e, errno);
            let mut orig = [0u8; 16];
            orig.copy_from_slice(&buf[..16]);
            if !sock.push_recv(super::netlink::build_nlmsg_error(errno, seq, pid, &orig)) {
                return Err(crate::utils::error::SyscallErr::ENOBUFS);
            }
        }
        return Ok(0);
    }

    let result = match msg_type {
        RTM_GETLINK => handle_getlink(seq, pid, sock),
        RTM_GETADDR => handle_getaddr(seq, pid, sock),
        RTM_GETROUTE => handle_getroute(seq, pid, sock),
        RTM_GETNEIGH => handle_getneigh(seq, pid, sock),
        _ => Err(crate::utils::error::SyscallErr::EOPNOTSUPP),
    };

    if let Err(e) = result {
        let errno = e as i32;
        log::warn!("[netlink] dump handler for msg_type={} failed: {:?} (errno={})", msg_type, e, errno);
        let mut orig = [0u8; 16];
        orig.copy_from_slice(&buf[..16]);
        if !sock.push_recv(super::netlink::build_nlmsg_error(errno, seq, pid, &orig)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
    }
    Ok(0)
}

fn handle_getlink(seq: u32, _req_pid: u32, sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let pid = sock.local_portid();
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
    // NLMSG_DONE = 20 bytes: 16-byte header + 4-byte zero error code
    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, seq, pid, &done_payload)) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

/// Handle non-dump RTM_GETLINK — specific-device lookup by ifindex or IFLA_IFNAME.
fn handle_getlink_single(seq: u32, pid: u32, buf: &[u8], sock: &NetlinkSocket) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    if payload.len() < 16 {
        let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
        if !sock.push_recv(super::netlink::build_nlmsg_error(22, seq, pid, &orig)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
        return Ok(0);
    }

    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let mut ifname: Option<alloc::string::String> = None;
    let mut offset = 16;
    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let rta_data = &payload[offset + 4..offset + rta_len];

        if rta_type == IFLA_IFNAME {
            let len = rta_data.iter().position(|&b| b == 0).unwrap_or(rta_data.len());
            ifname = Some(alloc::string::String::from(
                core::str::from_utf8(&rta_data[..len]).unwrap_or(""),
            ));
        }
        offset += (rta_len + 3) & !3;
    }

    let ns = crate::net::net_core::current_netns();
    let iface = if ifindex > 0 {
        ns.device_by_index(ifindex as usize)
    } else {
        ifname.as_ref().and_then(|n| ns.device_by_name(n))
    };

    let iface = match iface {
        Some(i) => i,
        None => {
            let mut orig = [0u8; 16]; orig.copy_from_slice(&buf[..16]);
            if !sock.push_recv(super::netlink::build_nlmsg_error(19, seq, pid, &orig)) {
                return Err(crate::utils::error::SyscallErr::ENOBUFS);
            }
            return Ok(0);
        }
    };

    let nic_id = iface.nic_id() as u32;
    let mut payload = alloc::vec::Vec::new();
    payload.push(0); payload.push(0);
    let ift = if nic_id == 1 { ARPHRD_LOOPBACK } else { ARPHRD_ETHER };
    payload.extend_from_slice(&ift.to_ne_bytes());
    payload.extend_from_slice(&nic_id.to_ne_bytes());
    payload.extend_from_slice(&iface.flags().to_ne_bytes());
    payload.extend_from_slice(&0u32.to_ne_bytes());
    let mut n = alloc::vec::Vec::new(); n.extend_from_slice(iface.iface_name().as_bytes()); n.push(0);
    payload.extend(&super::netlink::rta_data(IFLA_IFNAME, &n));
    payload.extend(&super::netlink::rta_data(IFLA_MTU, &(iface.mtu() as u32).to_ne_bytes()));
    if iface.kind() != DeviceKind::Loopback {
        payload.extend(&super::netlink::rta_data(IFLA_ADDRESS, &iface.mac()));
    }

    if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWLINK, 0, seq, pid, &payload)) {
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
            match cidr.address() {
                IpAddress::Ipv4(addr) => {
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
                IpAddress::Ipv6(addr) => {
                    let mut payload = Vec::new();
                    // ifaddrmsg: family(1) + prefixlen(1) + flags(1) + scope(1) + ifa_index(4) = 8 bytes
                    payload.push(10); // AF_INET6
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
    }
    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, seq, pid, &done_payload)) {
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
    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, seq, pid, &done_payload)) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

fn handle_getneigh(
    seq: u32,
    pid: u32,
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let entries = crate::net::neighbour::neighbour_dump();
    for (ifindex, ip, mac, state) in &entries {
        let mut payload = Vec::with_capacity(12 + 32);
        // ndmsg header (12 bytes)
        payload.push(2);        // ndm_family = AF_INET
        payload.push(0);        // ndm_pad1
        payload.push(0);        // ndm_pad2 low
        payload.push(0);        // ndm_pad2 high
        payload.extend_from_slice(&(*ifindex as i32).to_ne_bytes()); // ndm_ifindex
        payload.extend_from_slice(&state.to_ne_bytes()); // ndm_state
        payload.push(0);        // ndm_flags
        payload.push(0);        // ndm_type

        // NDA_DST: IP address
        if let IpAddress::Ipv4(a) = ip {
            payload.extend(&super::netlink::rta_data(NDA_DST, &a.0));
        } else {
            continue; // skip IPv6 for now
        }

        // NDA_LLADDR: MAC address
        payload.extend(&super::netlink::rta_data(NDA_LLADDR, &mac.0));

        if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWNEIGH, NLM_F_MULTI, seq, pid, &payload)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
    }
    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, seq, pid, &done_payload)) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

fn handle_delneigh(
    _seq: u32,
    _pid: u32,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    if payload.len() < 12 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }
    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]) as u32;
    let (dst_ip, _) = parse_nda_attrs(payload)?;
    let ip = dst_ip.ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    crate::net::neighbour::neighbour_delete(ifindex, ip);

    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, _seq, _pid, &done_payload)) {
        return Err(crate::utils::error::SyscallErr::ENOBUFS);
    }
    Ok(0)
}

fn handle_newneigh(
    _seq: u32,
    _pid: u32,
    buf: &[u8],
    _sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    if payload.len() < 12 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }
    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]) as u32;
    let (dst_ip, dst_mac) = parse_nda_attrs(payload)?;
    let ip = dst_ip.ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    let mac = dst_mac.ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    crate::net::neighbour::neighbour_record(ifindex, ip, mac);
    Ok(0)
}

/// Parse NDA_DST (IP) and NDA_LLADDR (MAC) from an ndmsg payload.
fn parse_nda_attrs(payload: &[u8]) -> Result<(Option<IpAddress>, Option<EthernetAddress>), crate::utils::error::SyscallErr> {
    let mut dst_ip: Option<IpAddress> = None;
    let mut dst_mac: Option<EthernetAddress> = None;
    let mut offset = 12;
    while offset + 4 <= payload.len() {
        let len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if len < 4 {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let data_start = offset + 4;
        let data_end = core::cmp::min(offset + len, payload.len());
        let data = &payload[data_start..data_end];

        match rta_type {
            NDA_DST if data.len() >= 4 => {
                dst_ip = Some(IpAddress::v4(data[0], data[1], data[2], data[3]));
            }
            NDA_LLADDR if data.len() >= 6 => {
                dst_mac = Some(EthernetAddress([data[0], data[1], data[2], data[3], data[4], data[5]]));
            }
            _ => {}
        }

        offset = super::netlink::nlmsg_align(offset + len);
    }
    Ok((dst_ip, dst_mac))
}

fn handle_getneigh_single(
    seq: u32,
    pid: u32,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    if payload.len() < 12 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }
    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]) as u32;
    let (dst_ip, _) = parse_nda_attrs(payload)?;
    let ip = dst_ip.ok_or(crate::utils::error::SyscallErr::EINVAL)?;

    // Look up the specific entry
    let table = crate::net::neighbour::NEIGHBOUR_TABLE.lock();
    if let Some(entry) = table.get(&(ifindex, ip)) {
        let mut payload_out = Vec::with_capacity(12 + 32);
        payload_out.push(2);        // ndm_family = AF_INET
        payload_out.push(0);
        payload_out.push(0);
        payload_out.push(0);
        payload_out.extend_from_slice(&(ifindex as i32).to_ne_bytes());
        payload_out.extend_from_slice(&entry.state.to_ne_bytes());
        payload_out.push(0);
        payload_out.push(0);
        if let IpAddress::Ipv4(a) = ip {
            payload_out.extend(&super::netlink::rta_data(NDA_DST, &a.0));
        }
        payload_out.extend(&super::netlink::rta_data(NDA_LLADDR, &entry.mac.0));
        if !sock.push_recv(super::netlink::build_nlmsg(RTM_NEWNEIGH, 0, seq, pid, &payload_out)) {
            return Err(crate::utils::error::SyscallErr::ENOBUFS);
        }
    }

    let done_payload = 0i32.to_ne_bytes();
    if !sock.push_recv(super::netlink::build_nlmsg(NLMSG_DONE, 0, seq, pid, &done_payload)) {
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
            // NLMSG_ERROR and NLMSG_DONE are terminal markers — never NLM_F_MULTI
            RouteNlSegment::Error(_) => {}
            RouteNlSegment::Done(_) => {}
        }
    }
}
