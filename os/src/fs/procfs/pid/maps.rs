//! /proc/<pid>/maps — 用户地址空间映射摘要。

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn pid_maps_content(
    pid: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };
    let vm = process.vm();
    let vm = vm.lock();
    let s = vm.proc_maps_content();
    proc_read_str(offset, len, buf, &s)
}
