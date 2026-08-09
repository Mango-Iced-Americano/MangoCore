//! /proc/<pid>/smaps -- minimal per-VMA memory accounting.

use crate::fs::vfs::SmapsCursor;
use crate::utils::error::SyscallErr;

pub fn pid_smaps_cursor(
    pid: usize,
    cursor: &mut SmapsCursor,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };
    let vm = process.vm();
    let copied = vm.read(|vm| vm.proc_smaps_read_cursor(cursor, offset, len, buf));
    Ok(copied)
}
