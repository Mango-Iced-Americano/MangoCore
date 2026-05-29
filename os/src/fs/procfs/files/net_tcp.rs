use alloc::string::String;
use crate::fs::procfs::proc_read_str;
use crate::net::Socket;
use crate::utils::error::SyscallErr;
use core::fmt::Write;

fn format_sock_addr(ip: smoltcp::wire::Ipv4Address, port: u16) -> String {
    let b = ip.as_bytes();
    let le_ip = (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24);
    alloc::format!("{:08X}:{:04X}", le_ip, port)
}

pub fn net_tcp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut content = String::new();
    content.push_str("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");

    let tcp_sockets = crate::net::TCP_SOCKETS.lock();
    for (idx, weak_sock) in tcp_sockets.iter().enumerate() {
        if let Some(socket) = weak_sock.upgrade() {
            let local = socket.local_endpoint();
            let remote = socket.remote_endpoint();

            let local_str = match &local {
                Some(crate::net::Endpoint::Ip(ep)) => match ep.addr {
                    smoltcp::wire::IpAddress::Ipv4(addr) => format_sock_addr(addr, ep.port),
                    _ => String::from("00000000:0000"),
                },
                _ => String::from("00000000:0000"),
            };
            let remote_str = match &remote {
                Some(crate::net::Endpoint::Ip(ep)) => match ep.addr {
                    smoltcp::wire::IpAddress::Ipv4(addr) => format_sock_addr(addr, ep.port),
                    _ => String::from("00000000:0000"),
                },
                _ => String::from("00000000:0000"),
            };
            let st = socket.tcp_state().map(|s| s as u8).unwrap_or_else(|| {
                if remote.is_some() { 1 } else { 10 }
            });

            let _ = write!(
                content,
                "{:>4}: {} {} {:02X} 00000000:00000000 00:00000000 00000000     0        0 0\n",
                idx, local_str, remote_str, st
            );
        }
    }

    proc_read_str(offset, len, buf, &content)
}
