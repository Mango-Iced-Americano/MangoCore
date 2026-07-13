//! Dynamic resolver configuration exported from the active DHCP lease.

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::fmt::Write;
use alloc::string::String;

pub fn net_resolv_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let servers = crate::net::net_core::dns_servers();
    let mut content = String::new();
    if servers.is_empty() {
        // Preserve the QEMU SLIRP fallback until DHCP publishes its lease.
        content.push_str("nameserver 10.0.2.3\n");
    } else {
        for server in servers {
            let _ = writeln!(content, "nameserver {}", server);
        }
    }
    proc_read_str(offset, len, buf, &content)
}
