//! /proc/sysvipc/* — SysV IPC registry snapshots used by LTP probes.

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn shm_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let snapshot = crate::syscall::sysv_shm_proc_snapshot();
    proc_read_str(offset, len, buf, &snapshot)
}

pub fn msg_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let snapshot = crate::syscall::sysv_msg_proc_snapshot();
    proc_read_str(offset, len, buf, &snapshot)
}

pub fn sem_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let snapshot = crate::syscall::sysv_sem_proc_snapshot();
    proc_read_str(offset, len, buf, &snapshot)
}
