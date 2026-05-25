//! /proc/sys/* — LTP 环境探测所需的最小兼容节点。

use alloc::format;
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

pub fn ns_last_pid_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::task::ns_last_pid());
    proc_read_str(offset, len, buf, &value)
}

pub fn ns_last_pid_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let value = text
        .trim()
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    crate::task::set_ns_last_pid(value);
    Ok(buf.len())
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
