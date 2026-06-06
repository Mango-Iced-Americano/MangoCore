use alloc::string::String;
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn net_arp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let content = String::from(
        "IP address       HW type     Flags       HW address            Mask     Device\n",
    );
    proc_read_str(offset, len, buf, &content)
}
