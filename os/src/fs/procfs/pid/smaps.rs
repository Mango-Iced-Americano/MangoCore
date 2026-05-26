//! /proc/<pid>/smaps -- minimal per-VMA memory accounting.

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;

pub fn pid_smaps_content(
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
    let s = vm.lock().proc_smaps_content();
    proc_read_str(offset, len, buf, &s)
}
