//! /proc/sys/* — LTP 环境探测所需的最小兼容节点。

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn pid_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "32768\n")
}

pub fn tainted_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn max_user_namespaces_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn osrelease_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "5.10.0-mangocore\n")
}

pub fn net_conf_tag_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}
