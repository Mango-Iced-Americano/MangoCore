//! /proc/sys/* — LTP 环境探测所需的最小兼容节点。

use alloc::format;
use alloc::string::{String, ToString};
use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    static ref CORE_PATTERN: Mutex<String> = Mutex::new(String::from("core\n"));
}

pub fn pid_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "32768\n")
}

pub fn threads_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::config::SYSTEM_TASK_LIMIT);
    proc_read_str(offset, len, buf, &value)
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

pub fn core_pattern_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let pattern = CORE_PATTERN.lock();
    proc_read_str(offset, len, buf, &pattern)
}

pub fn core_pattern_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    *CORE_PATTERN.lock() = text.to_string();
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

pub fn shmmax_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmmax());
    proc_read_str(offset, len, buf, &value)
}

pub fn shmall_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmall());
    proc_read_str(offset, len, buf, &value)
}

pub fn shmmni_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_shmmni());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmax_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmax());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmax_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmax(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msgmnb_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmnb());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmnb_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmnb(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msgmni_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msgmni());
    proc_read_str(offset, len, buf, &value)
}

pub fn msgmni_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_msgmni(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn msg_next_id_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::sysv_msg_next_id());
    proc_read_str(offset, len, buf, &value)
}

pub fn msg_next_id_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let value = text
        .trim()
        .parse::<i32>()
        .map_err(|_| SyscallErr::EINVAL)?;
    if !crate::syscall::set_sysv_msg_next_id(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

fn parse_usize_sysctl(buf: &[u8]) -> Result<usize, SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    text.trim()
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)
}

pub fn net_conf_tag_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn ip_forward_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn ip_forward_write(
    _extra: usize,
    _offset: usize,
    _buf: &[u8],
) -> Result<usize, SyscallErr> {
    Err(SyscallErr::EPERM)
}
