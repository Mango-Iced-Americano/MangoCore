//! /proc/filesystems — 列出当前内核支持的文件系统

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;
use core::fmt::Write;

pub fn filesystems_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(256);
    let _ = writeln!(s, "nodev\tproc");
    let _ = writeln!(s, "nodev\tdevfs");
    let _ = writeln!(s, "nodev\tramfs");
    let _ = writeln!(s, "nodev\ttmpfs");
    let _ = writeln!(s, "\text4");
    let _ = writeln!(s, "\tvfat");
    proc_read_str(offset, len, buf, &s)
}
