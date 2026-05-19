//! /proc/<pid>/cmdline — 进程命令行

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::{String, ToString};

pub fn pid_cmdline_content(
    pid: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };
    let task = match process
        .any_live_thread()
        .or_else(|| process.threads().into_iter().next())
    {
        Some(task) => task,
        None => return Err(SyscallErr::ENOENT),
    };

    let exe_path = task.exe_path.lock().clone();
    let s = if exe_path.is_empty() {
        String::from("initproc")
    } else {
        // Extract just the binary name for simplicity
        if let Some(pos) = exe_path.rfind('/') {
            exe_path[pos + 1..].to_string()
        } else {
            exe_path
        }
    };

    // cmdline format: argument\0argument\0...
    // Simplified to just the program name
    let mut out = String::with_capacity(s.len() + 1);
    out.push_str(&s);
    out.push('\0');

    proc_read_str(offset, len, buf, &out)
}
