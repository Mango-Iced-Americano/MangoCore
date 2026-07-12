//! /proc/uptime — 系统运行时间

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::format;

pub fn uptime_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let uptime = crate::timer::uptime();
    let s = format!("{}.00 0.00\n", uptime);
    proc_read_str(offset, len, buf, &s)
}
