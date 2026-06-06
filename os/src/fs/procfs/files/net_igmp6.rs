//! /proc/net/igmp6 — IPv6 multicast group membership (for netstat -gn)
//!
//! Format matches net-tools igmp_do_one() IPv6 parser:
//!   No header line (first line must NOT contain "Device" to set igmp6_flag).
//!   Each line: "%d %15s %64[0-9A-Fa-f] %d" → idx, device, addr, refcnt.
//!   Addr must be flat hex (no colons) for %64[0-9A-Fa-f] to match.

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn net_igmp6_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let content = concat!(
        "1 lo ff020000000000000000000000000001 1\n",
        "2 eth0 ff020000000000000000000000000001 1\n",
        "2 eth0 ff010000000000000000000000000001 1\n",
    );
    proc_read_str(offset, len, buf, content)
}
