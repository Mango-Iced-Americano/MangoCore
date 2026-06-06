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

pub fn pipe_max_size_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_max_size());
    proc_read_str(offset, len, buf, &value)
}

pub fn pipe_max_size_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::fs::dev::pipe::set_pipe_max_size(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn pipe_user_pages_soft_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_user_pages_soft());
    proc_read_str(offset, len, buf, &value)
}

pub fn pipe_user_pages_hard_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::fs::dev::pipe::pipe_user_pages_hard());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_queues_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_queues_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_queues_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_queues_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msg_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msg_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msg_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msg_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msgsize_max_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msgsize_max());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msgsize_max_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msgsize_max(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msg_default_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msg_default());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msg_default_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msg_default(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn mqueue_msgsize_default_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::syscall::posix_mq_msgsize_default());
    proc_read_str(offset, len, buf, &value)
}

pub fn mqueue_msgsize_default_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::syscall::set_posix_mq_msgsize_default(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn overcommit_memory_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::overcommit_memory());
    proc_read_str(offset, len, buf, &value)
}

pub fn overcommit_memory_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::mm::set_overcommit_memory(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn overcommit_ratio_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::overcommit_ratio());
    proc_read_str(offset, len, buf, &value)
}

pub fn overcommit_ratio_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_overcommit_ratio(value);
    Ok(buf.len())
}

pub fn max_map_count_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::max_map_count());
    proc_read_str(offset, len, buf, &value)
}

pub fn max_map_count_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    if !crate::mm::set_max_map_count(value) {
        return Err(SyscallErr::EINVAL);
    }
    Ok(buf.len())
}

pub fn min_free_kbytes_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::min_free_kbytes());
    proc_read_str(offset, len, buf, &value)
}

pub fn min_free_kbytes_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_min_free_kbytes(value);
    Ok(buf.len())
}

pub fn panic_on_oom_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let value = format!("{}\n", crate::mm::panic_on_oom());
    proc_read_str(offset, len, buf, &value)
}

pub fn panic_on_oom_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let value = parse_usize_sysctl(buf)?;
    crate::mm::set_panic_on_oom(value);
    Ok(buf.len())
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

pub fn sem_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let (semmsl, semmns, semopm, semmni) = crate::syscall::sysv_sem_limits();
    let value = format!("{semmsl}\t{semmns}\t{semopm}\t{semmni}\n");
    proc_read_str(offset, len, buf, &value)
}

pub fn sem_write(
    _extra: usize,
    _offset: usize,
    buf: &[u8],
) -> Result<usize, SyscallErr> {
    let (semmsl, semmns, semopm, semmni) = parse_four_usize_sysctl(buf)?;
    if !crate::syscall::set_sysv_sem_limits(semmsl, semmns, semopm, semmni) {
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

fn parse_four_usize_sysctl(buf: &[u8]) -> Result<(usize, usize, usize, usize), SyscallErr> {
    let text = core::str::from_utf8(buf).map_err(|_| SyscallErr::EINVAL)?;
    let mut fields = text.split_whitespace();
    let semmsl = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semmns = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semopm = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    let semmni = fields
        .next()
        .ok_or(SyscallErr::EINVAL)?
        .parse::<usize>()
        .map_err(|_| SyscallErr::EINVAL)?;
    Ok((semmsl, semmns, semopm, semmni))
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

pub fn disable_ipv6_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "0\n")
}

pub fn net_snmp_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "Ip: Forwarding DefaultTTL InReceives\nIp: 2 64 0\n")
}

pub fn net_netstat_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "TcpExt: SyncookiesSent\nTcpExt: 0\n")
}

pub fn net_snmp6_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    proc_read_str(offset, len, buf, "Ip6: InReceives\nIp6: 0\n")
}
