//! /proc/net/route — 路由表（小端格式）

use alloc::format;
use alloc::string::{String, ToString};
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use smoltcp::wire::Ipv4Address;

fn ip_to_le_hex(ip: Ipv4Address) -> String {
    let b = ip.as_bytes();
    format!("{:02X}{:02X}{:02X}{:02X}", b[3], b[2], b[1], b[0])
}

pub fn net_route_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut content = String::new();
    content.push_str("Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n");

    let router = crate::net::routing::Router::init_default();
    for entry in &router.table.entries {
        let ifname = crate::net::net_core::find_by_index(entry.ifindex)
            .map(|d| d.name.to_string())
            .unwrap_or_else(|| String::from("?"));

        let dest = match entry.destination.address() {
            smoltcp::wire::IpAddress::Ipv4(addr) => ip_to_le_hex(addr),
            _ => String::from("00000000"),
        };
        let gateway = match entry.next_hop {
            Some(smoltcp::wire::IpAddress::Ipv4(addr)) => ip_to_le_hex(addr),
            _ => String::from("00000000"),
        };
        let prefix = entry.destination.prefix_len();
        let mask_u32: u32 = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
        let mask_str = format!("{:08X}", u32::to_le(mask_u32));
        let flags = match entry.route_type {
            crate::net::routing::RouteType::Default => "0003",
            _ => "0001",
        };
        let mtu = crate::net::net_core::find_by_index(entry.ifindex)
            .map(|d| d.mtu)
            .unwrap_or(0);

        content.push_str(&format!(
            "{}\t{}\t{}\t{}\t0\t0\t{}\t{}\t{}\t0\t0\n",
            ifname, dest, gateway, flags, entry.metric, mask_str, mtu,
        ));
    }

    proc_read_str(offset, len, buf, &content)
}
