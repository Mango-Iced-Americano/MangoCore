//! /proc/<pid>/exe — dynamic symlink to process executable

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;

pub fn pid_exe_content(
    pid: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let path = match crate::task::find_process_by_pid(pid) {
        Some(task) => task.exe_path(),
        None => String::new(),
    };
    proc_read_str(offset, len, buf, &path)
}
