use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::net::iface::Iface;
use crate::utils::error::SyscallErr;

use super::super::NetlinkSocket;
use super::super::netlink::{
    IFLA_IFNAME, IFLA_LINKINFO, IFLA_INFO_KIND, IFLA_INFO_DATA,
    IFLA_MTU, IFLA_NET_NS_PID,
    NLA_F_NESTED, NLM_F_CREATE, NLM_F_EXCL,
    VETH_INFO_PEER,
};

pub fn handle_newlink(
    seq: u32,
    pid: u32,
    buf: &[u8],
    flags: u16,
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    log::warn!("[netlink] handle_newlink called: seq={} pid={} flags={:#x} buf_len={}", seq, pid, flags, buf.len());
    let payload = &buf[16..];
    if payload.len() < 16 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }

    let _family = payload[0];
    let _ifitype = u16::from_ne_bytes([payload[2], payload[3]]);
    let ifindex = u32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let mut ifname: Option<String> = None;
    let mut peer_name: Option<String> = None;
    let mut linkkind: Option<String> = None;
    let mut target_netns_pid: Option<u32> = None;
    let mut linkinfo_is_nested = false;
    let mut offset = 16;

    // ---- Walk top-level RTA attributes ----
    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type_raw = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let rta_type = rta_type_raw & !NLA_F_NESTED;
        let rta_payload = &payload[offset + 4..offset + rta_len];

        match rta_type {
            IFLA_IFNAME => {
                let len = rta_payload.iter().position(|&b| b == 0).unwrap_or(rta_payload.len());
                ifname = Some(String::from(core::str::from_utf8(&rta_payload[..len]).unwrap_or("")));
            }
            IFLA_NET_NS_PID if rta_payload.len() >= 4 => {
                target_netns_pid = Some(u32::from_ne_bytes([
                    rta_payload[0], rta_payload[1], rta_payload[2], rta_payload[3],
                ]));
            }
            IFLA_LINKINFO => {
                linkinfo_is_nested = (rta_type_raw & NLA_F_NESTED) != 0;
                // ---- Walk IFLA_LINKINFO nested attributes: IFLA_INFO_KIND, IFLA_INFO_DATA ----
                let mut loff = 0;
                while loff + 4 <= rta_payload.len() {
                    let l_len = u16::from_ne_bytes([rta_payload[loff], rta_payload[loff + 1]]) as usize;
                    if l_len < 4 || loff + l_len > rta_payload.len() {
                        break;
                    }
                    let l_type_raw = u16::from_ne_bytes([rta_payload[loff + 2], rta_payload[loff + 3]]);
                    let l_type = l_type_raw & !NLA_F_NESTED;
                    let l_payload = &rta_payload[loff + 4..loff + l_len];

                    match l_type {
                        IFLA_INFO_KIND => {
                            let len = l_payload.iter().position(|&b| b == 0).unwrap_or(l_payload.len());
                            linkkind = Some(String::from(
                                core::str::from_utf8(&l_payload[..len]).unwrap_or(""),
                            ));
                        }
                        IFLA_INFO_DATA => {
                            // ---- Walk VETH-specific nested data: VETH_INFO_PEER ----
                            let mut ploff = 0;
                            while ploff + 4 <= l_payload.len() {
                                let p_len = u16::from_ne_bytes(
                                    [l_payload[ploff], l_payload[ploff + 1]],
                                ) as usize;
                                if p_len < 4 || ploff + p_len > l_payload.len() {
                                    break;
                                }
                                let p_type_raw = u16::from_ne_bytes(
                                    [l_payload[ploff + 2], l_payload[ploff + 3]],
                                );
                                let p_type = p_type_raw & !NLA_F_NESTED;
                                let p_payload = &l_payload[ploff + 4..ploff + p_len];

                                if p_type == VETH_INFO_PEER {
                                    // Peer: nested ifinfomsg(16 bytes) + RTA attributes
                                    if p_payload.len() >= 16 {
                                        let mut poff = 16;
                                        while poff + 4 <= p_payload.len() {
                                            let pp_len = u16::from_ne_bytes(
                                                [p_payload[poff], p_payload[poff + 1]],
                                            ) as usize;
                                            if pp_len < 4 || poff + pp_len > p_payload.len() {
                                                break;
                                            }
                                            let pp_type_raw = u16::from_ne_bytes(
                                                [p_payload[poff + 2], p_payload[poff + 3]],
                                            );
                                            let pp_type = pp_type_raw & !NLA_F_NESTED;
                                            let pp_data = &p_payload[poff + 4..poff + pp_len];

                                            if pp_type == IFLA_IFNAME {
                                                let len = pp_data
                                                    .iter()
                                                    .position(|&b| b == 0)
                                                    .unwrap_or(pp_data.len());
                                                peer_name = Some(String::from(
                                                    core::str::from_utf8(&pp_data[..len])
                                                        .unwrap_or(""),
                                                ));
                                            }
                                            poff += (pp_len + 3) & !3;
                                        }
                                    }
                                }
                                ploff += (p_len + 3) & !3;
                            }
                        }
                        _ => {}
                    }
                    loff += (l_len + 3) & !3;
                }
            }
            _ => {}
        }
        offset += (rta_len + 3) & !3;
    }

    // ---- Dispatch: move to another netns (IFLA_NET_NS_PID) ----
    if let Some(target_pid) = target_netns_pid {
        let ns = crate::net::net_core::current_netns();
        let iface = if ifindex > 0 {
            ns.device_by_index(ifindex as usize)
        } else {
            ifname.as_ref().and_then(|n| ns.device_by_name(n))
        };
        let iface = match iface {
            Some(i) => i,
            None => {
                send_error(sock, buf, seq, pid, 19)?; // ENODEV
                return Ok(0);
            }
        };
        if iface.kind() == crate::net::iface::DeviceKind::Loopback {
            send_error(sock, buf, seq, pid, 16)?; // EBUSY
            return Ok(0);
        }
        let target_proc = match crate::task::find_process_by_pid(target_pid as usize) {
            Some(p) => p,
            None => {
                send_error(sock, buf, seq, pid, 3)?; // ESRCH
                return Ok(0);
            }
        };
        let dst_ns = target_proc.net();
        ns.remove_device(iface.nic_id());
        iface.common().net_namespace.write().replace(Arc::downgrade(&dst_ns));
        dst_ns.add_device(iface);
        send_ack(sock, buf, seq, pid)?;
        return Ok(0);
    }

    // ---- Dispatch: existing-link modification (no IFLA_INFO_KIND) ----
    let kind = linkkind.as_deref().unwrap_or("");
    log::info!("[netlink] handle_newlink: kind='{}' ifindex={} ifname={:?} peer={:?}", kind, ifindex, ifname, peer_name);
    if kind.is_empty() && (ifindex > 0 || ifname.is_some()) {
        return handle_setlink(seq, pid, buf, sock);
    }

    // ---- Validate link kind (pure create path) ----
    if kind.is_empty() {
        send_error(sock, buf, seq, pid, 22)?;
        return Ok(0);
    }
    if kind != "veth" {
        send_error(sock, buf, seq, pid, 95)?; // EOPNOTSUPP: unsupported kind (e.g. "bridge")
        return Ok(0);
    }

    let name1 = match ifname {
        Some(ref n) if !n.is_empty() => n.clone(),
        _ => {
            send_error(sock, buf, seq, pid, 22)?; // EINVAL: missing IFLA_IFNAME
            return Ok(0);
        }
    };
    let name2 = match peer_name {
        Some(ref n) if !n.is_empty() => n.clone(),
        _ => infer_veth_peer_name(&name1),
    };

    // ---- Check for name conflicts in current namespace ----
    if crate::net::net_core::find_by_name(&name1).is_some()
        || crate::net::net_core::find_by_name(&name2).is_some()
    {
        send_error(sock, buf, seq, pid, 17)?;
        return Ok(0);
    }

    // ---- Create veth pair ----
    let (ifidx1, ifidx2) = crate::drivers::net::veth::veth_pair_new(&name1, &name2);
    log::info!(
        "[netlink] created veth pair: {} (ifindex={}) <-> {} (ifindex={})",
        name1, ifidx1, name2, ifidx2
    );

    // ---- Send ACK —— rollback on failure ----
    if let Err(e) = send_ack(sock, buf, seq, pid) {
        let ns = crate::net::net_core::current_netns();
        if let Some(iface) = ns.device_by_index(ifidx1 as usize) {
            crate::drivers::net::veth::veth_pair_delete(iface);
        }
        return Err(e);
    }
    Ok(0)
}


fn send_ack(sock: &NetlinkSocket, buf: &[u8], seq: u32, pid: u32) -> Result<(), SyscallErr> {
    let mut orig = [0u8; 16];
    orig.copy_from_slice(&buf[..16]);
    if !sock.push_recv(super::super::netlink::build_nlmsg_error(0, seq, pid, &orig)) {
        return Err(SyscallErr::ENOBUFS);
    }
    Ok(())
}

fn send_error(sock: &NetlinkSocket, buf: &[u8], seq: u32, pid: u32, errno: i32) -> Result<(), SyscallErr> {
    let mut orig = [0u8; 16];
    orig.copy_from_slice(&buf[..16]);
    if !sock.push_recv(super::super::netlink::build_nlmsg_error(errno, seq, pid, &orig)) {
        return Err(SyscallErr::ENOBUFS);
    }
    Ok(())
}

/// Look up an interface by its numeric index.
fn find_iface_by_index(index: i32) -> Option<Arc<dyn Iface>> {
    let ns = crate::net::net_core::current_netns();
    let list = ns.device_list.lock();
    list.values()
        .find(|iface| iface.nic_id() as i32 == index)
        .cloned()
}

/// Parse a NUL-terminated string from an RTA attribute payload.
fn parse_string(data: &[u8]) -> String {
    let len = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from(core::str::from_utf8(&data[..len]).unwrap_or(""))
}

/// Handle `RTM_SETLINK` — modify device configuration.
///
/// Supported operations:
/// - IFF_UP / IFF_DOWN (via `ifinfomsg.flags` + `ifinfomsg.change` mask)
/// - IFLA_IFNAME → rename (checked for uniqueness within the namespace)
/// - IFLA_MTU → set MTU
///
/// Lookup is by interface index from ifinfomsg, or by IFLA_IFNAME if index is 0.
pub fn handle_setlink(
    seq: u32,
    pid: u32,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    // ifinfomsg is 16 bytes: family(1) + pad(1) + type(2) + index(4) + flags(4) + change(4)
    if payload.len() < 16 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }

    let _family = payload[0];
    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let req_flags = u32::from_ne_bytes([payload[8], payload[9], payload[10], payload[11]]);
    let req_change = u32::from_ne_bytes([payload[12], payload[13], payload[14], payload[15]]);

    // Parse RTA attributes starting at offset 16
    let mut new_name: Option<String> = None;
    let mut new_mtu: Option<usize> = None;
    let mut name_filter: Option<String> = None;
    let mut offset = 16;

    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let rta_data = &payload[offset + 4..offset + rta_len];

        match rta_type {
            IFLA_IFNAME => {
                let s = parse_string(rta_data);
                if ifindex == 0 {
                    name_filter = Some(s.clone());
                }
                new_name = Some(s);
            }
            IFLA_MTU if rta_data.len() >= 4 => {
                new_mtu = Some(
                    u32::from_ne_bytes([rta_data[0], rta_data[1], rta_data[2], rta_data[3]])
                        as usize,
                );
            }
            _ => {}
        }
        offset += (rta_len + 3) & !3;
    }

    // Look up the device by index, or by name if index is 0
    let ns = crate::net::net_core::current_netns();
    let iface = if ifindex > 0 {
        ns.device_by_index(ifindex as usize)
    } else {
        name_filter.as_ref().and_then(|name| ns.device_by_name(name))
    };

    let iface = match iface {
        Some(i) => i,
        None => {
            send_error(sock, buf, seq, pid, 19)?; // ENODEV
            return Ok(0);
        }
    };

    // Apply flag changes: only bits set in req_change are updated from req_flags
    if req_change != 0 {
        let old_flags = iface.flags();
        let new_flags = (old_flags & !req_change) | (req_flags & req_change);
        iface.set_flags(new_flags);
    }

    if let Some(ref name) = new_name {
        if iface.iface_name() != *name {
            let exists = {
                let list = ns.device_list.lock();
                list.values()
                    .any(|d| d.nic_id() != iface.nic_id() && d.iface_name() == *name)
            };
            if exists {
                send_error(sock, buf, seq, pid, 17)?; // EEXIST
                return Ok(0);
            }
            iface.set_iface_name(name);
        }
    }

    if let Some(mtu) = new_mtu {
        iface.set_mtu(mtu);
    }

    send_ack(sock, buf, seq, pid)?;
    Ok(0)
}

/// Handle `RTM_DELLINK` — delete a network device.
pub fn handle_dellink(
    seq: u32,
    pid: u32,
    buf: &[u8],
    sock: &NetlinkSocket,
) -> Result<isize, crate::utils::error::SyscallErr> {
    let payload = buf.get(16..).ok_or(crate::utils::error::SyscallErr::EINVAL)?;
    if payload.len() < 16 {
        return Err(crate::utils::error::SyscallErr::EINVAL);
    }

    let _family = payload[0];
    let ifindex = i32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]);

    let mut ifname: Option<String> = None;
    let mut offset = 16;
    while offset + 4 <= payload.len() {
        let rta_len = u16::from_ne_bytes([payload[offset], payload[offset + 1]]) as usize;
        if rta_len < 4 || offset + rta_len > payload.len() {
            break;
        }
        let rta_type = u16::from_ne_bytes([payload[offset + 2], payload[offset + 3]]);
        let rta_data = &payload[offset + 4..offset + rta_len];

        if rta_type == IFLA_IFNAME {
            ifname = Some(parse_string(rta_data));
        }
        offset += (rta_len + 3) & !3;
    }

    let ns = crate::net::net_core::current_netns();
    let iface = if ifindex > 0 {
        ns.device_by_index(ifindex as usize)
    } else {
        ifname.as_ref().and_then(|name| ns.device_by_name(name))
    };

    let iface = match iface {
        Some(i) => i,
        None => {
            send_error(sock, buf, seq, pid, 19)?;
            return Ok(0);
        }
    };

    match iface.kind() {
        crate::net::iface::DeviceKind::Loopback => {
            send_error(sock, buf, seq, pid, 95)?;
        }
        crate::net::iface::DeviceKind::Veth => {
            crate::drivers::net::veth::veth_pair_delete(iface);
            send_ack(sock, buf, seq, pid)?;
        }
        _ => {
            send_error(sock, buf, seq, pid, 95)?;
        }
    }

    Ok(0)
}

fn infer_veth_peer_name(name: &str) -> String {
    // Find the trailing digit sequence (e.g., "veth_t01" -> "veth_t02", "eth0" -> "eth1").
    let bytes = name.as_bytes();
    let digit_len = bytes.iter().rev().take_while(|b| b.is_ascii_digit()).count();
    if digit_len > 0 {
        let split = name.len() - digit_len;
        let prefix = &name[..split];
        let suffix = &name[split..];
        if let Ok(num) = suffix.parse::<u64>() {
            if let Some(next) = num.checked_add(1) {
                let next_str = alloc::format!("{:0width$}", next, width = suffix.len());
                let candidate = alloc::string::ToString::to_string(prefix) + &next_str;
                if crate::net::net_core::find_by_name(&candidate).is_none() {
                    return candidate;
                }
            }
        }
    }
    for i in 0u32..4096 {
        let candidate = alloc::format!("veth{}", i);
        if crate::net::net_core::find_by_name(&candidate).is_none() {
            return candidate;
        }
    }
    alloc::format!("veth{}", crate::net::net_core::next_ifindex())
}
