//! /proc/net/igmp — IPv4 multicast group membership (for netstat -gn)
//!
//! Format matches net-tools igmp_do_one() parser expectations:
//!   Line 0: "Idx\tDevice\tCount\tQuerier\tGroup" — triggers IPv4 + idx_flag
//!   Device lines: "%d\t%-8s\t%d\tV3\t" → parsed as "%d\t%15c"
//!   Group lines: "\t%08X\t%d" → multicast addr + refcount

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn net_igmp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let content = concat!(
        "Idx\tDevice\tCount\tQuerier\tGroup\n",
        "1\tlo      \t0\tV3\t\n",
        "\tE0000001\t1\n",
        "2\teth0    \t1\tV3\t\n",
        "\tE00000FB\t1\n",
        "\tE0000016\t1\n",
    );
    proc_read_str(offset, len, buf, content)
}
