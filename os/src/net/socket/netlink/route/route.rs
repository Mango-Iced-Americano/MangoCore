use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use crate::utils::error::SyscallErr;

use super::super::netlink::{
    build_nlmsg_error, NLM_F_CREATE, NLM_F_EXCL, NLM_F_REPLACE, RTA_DST, RTA_GATEWAY, RTA_OIF,
};
use super::super::NetlinkSocket;

/// rtmsg fields parsed from the 12-byte wire header.
struct RtMsg {
    dst_len: u8,
    type_: u8,
}

fn parse_rtmsg(payload: &[u8]) -> Option<RtMsg> {
    if payload.len() < 12 {
        return None;
    }
    Some(RtMsg {
        dst_len: payload[1],
        type_: payload[7],
    })
}

/// Parsed RTA attributes for route operations.
struct RouteAttrs {
    dst: Option<Ipv4Address>,
    gateway: Option<Ipv4Address>,
    oif: Option<u32>,
}

fn parse_route_attrs(payload: &[u8], mut offset: usize) -> RouteAttrs {
    let mut dst = None;
    let mut gateway = None;
    let mut oif = None;

    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let rta_data = &payload[offset + 4..offset + rta_len];

        match rta_type {
            RTA_DST if rta_data.len() >= 4 => {
                dst = Some(Ipv4Address([
                    rta_data[0],
                    rta_data[1],
                    rta_data[2],
                    rta_data[3],
                ]));
            }
            RTA_GATEWAY if rta_data.len() >= 4 => {
                gateway = Some(Ipv4Address([
                    rta_data[0],
                    rta_data[1],
                    rta_data[2],
                    rta_data[3],
                ]));
            }
            RTA_OIF if rta_data.len() >= 4 => {
                oif = Some(u32::from_ne_bytes([
                    rta_data[0],
                    rta_data[1],
                    rta_data[2],
                    rta_data[3],
                ]));
            }
            _ => {}
        }

        offset += (rta_len + 3) & !3;
    }

    RouteAttrs { dst, gateway, oif }
}

fn build_cidr(dst_len: u8, dst_addr: Option<Ipv4Address>) -> Option<IpCidr> {
    if dst_len > 0 {
        dst_addr.map(|addr| IpCidr::new(IpAddress::Ipv4(addr), dst_len))
    } else {
        Some(IpCidr::new(IpAddress::v4(0, 0, 0, 0), 0))
    }
}

fn send_ack(
    sock: &NetlinkSocket,
    buf: &[u8],
    seq: u32,
    pid: u32,
    errno: i32,
) -> Result<(), SyscallErr> {
    let mut orig = [0u8; 16];
    orig.copy_from_slice(&buf[..16]);
    if !sock.push_recv(build_nlmsg_error(errno, seq, pid, &orig)) {
        return Err(SyscallErr::ENOBUFS);
    }
    Ok(())
}

pub fn handle_newroute(
    seq: u32,
    pid: u32,
    buf: &[u8],
    flags: u16,
    sock: &NetlinkSocket,
) -> Result<isize, SyscallErr> {
    let payload = buf.get(16..).ok_or(SyscallErr::EINVAL)?;
    let rtm = parse_rtmsg(payload).ok_or(SyscallErr::EINVAL)?;

    let attrs = parse_route_attrs(payload, 12);

    let ifindex = match attrs.oif {
        Some(idx) => idx,
        None => {
            send_ack(sock, buf, seq, pid, 22)?;
            return Ok(0);
        }
    };

    let dest_cidr = match build_cidr(rtm.dst_len, attrs.dst) {
        Some(c) => c,
        None => {
            send_ack(sock, buf, seq, pid, 22)?;
            return Ok(0);
        }
    };

    let route_type = if rtm.type_ == 3 {
        crate::net::routing::RouteType::Default
    } else {
        crate::net::routing::RouteType::Static
    };

    let next_hop = attrs.gateway.map(|gw| IpAddress::Ipv4(gw));

    let ns = crate::net::net_core::current_netns();
    let mut router = ns.router.lock();
    let has_existing = router
        .table
        .entries
        .iter()
        .any(|e| e.destination == dest_cidr);

    if flags & NLM_F_EXCL != 0 && has_existing {
        send_ack(sock, buf, seq, pid, 17)?;
        return Ok(0);
    }

    if flags & NLM_F_REPLACE != 0 {
        router.remove_route(&dest_cidr);
    }

    router.add_route(dest_cidr, next_hop, ifindex, 0, route_type);
    drop(router);

    send_ack(sock, buf, seq, pid, 0)?;
    Ok(0)
}

pub fn handle_delroute(
    seq: u32,
    pid: u32,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, SyscallErr> {
    let payload = buf.get(16..).ok_or(SyscallErr::EINVAL)?;
    let rtm = parse_rtmsg(payload).ok_or(SyscallErr::EINVAL)?;

    let attrs = parse_route_attrs(payload, 12);

    let dest_cidr = match build_cidr(rtm.dst_len, attrs.dst) {
        Some(c) => c,
        None => {
            send_ack(sock, buf, seq, pid, 22)?;
            return Ok(0);
        }
    };

    let ns = crate::net::net_core::current_netns();
    let mut router = ns.router.lock();
    router.remove_route(&dest_cidr);
    drop(router);

    send_ack(sock, buf, seq, pid, 0)?;
    Ok(0)
}
