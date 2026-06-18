//! /proc/<pid>/smaps -- minimal per-VMA memory accounting.

use alloc::string::String;

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
    let copied = vm.lock().proc_smaps_read(offset, len, buf);
    Ok(copied)
}

pub fn pid_smaps_snapshot(pid: usize) -> Result<String, SyscallErr> {
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };
    let vm = process.vm();
    let content = vm.lock().proc_smaps_content();
    Ok(content)
}
