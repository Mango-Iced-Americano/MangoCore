//! /proc/cmdline — kernel command line

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn cmdline_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "BOOT_IMAGE=kernel\n")
}
