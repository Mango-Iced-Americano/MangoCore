use alloc::string::String;
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use smoltcp::wire::IpAddress;

pub fn net_arp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut content = String::from(
        "IP address       HW type     Flags       HW address            Mask     Device\n",
    );

    let entries = crate::net::neighbour::neighbour_dump();
    for (ifindex, ip, mac, _state) in &entries {
        if let IpAddress::Ipv4(a) = ip {
            let dev_name = crate::net::net_core::current_netns()
                .device_list.lock()
                .iter()
                .find(|(_, iface)| iface.nic_id() as u32 == *ifindex)
                .map(|(_, iface)| iface.iface_name().clone())
                .unwrap_or_else(|| alloc::string::String::from("?"));
            content.push_str(&alloc::format!(
                "{:<15} 0x1         0x2         {:<22} *        {}\n",
                alloc::format!("{}.{}.{}.{}", a.0[0], a.0[1], a.0[2], a.0[3]),
                alloc::format!(
                    "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    mac.0[0], mac.0[1], mac.0[2], mac.0[3], mac.0[4], mac.0[5]
                ),
                dev_name,
            ));
        }
    }

    proc_read_str(offset, len, buf, &content)
}
