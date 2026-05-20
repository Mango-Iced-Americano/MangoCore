//! /proc/<pid>/stat — 进程状态（Linux procfs 兼容格式）
//!
//! 仿照 DragonOS /proc/<pid>/stat 设计。

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::String;
use crate::task::TaskStatus;

pub fn pid_stat_content(
    pid: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };

    let state_char = if process.is_zombie() {
        'Z'
    } else if let Some(task) = process.any_live_thread() {
        let inner = task.acquire_inner_lock();
        match inner.task_status {
            TaskStatus::Ready => 'R',
            TaskStatus::Running => 'R',
            TaskStatus::Interruptible => 'S',
            TaskStatus::Zombie => 'Z',
        }
    } else {
        'R'
    };
    let ppid = process.parent_pid();
    let pgid = process.getpgid();
    let num_threads = process.live_thread_count().max(1);

    let comm = {
        let exe = process.exe_path();
        if exe.is_empty() {
            String::from("(initproc)")
        } else if let Some(pos) = exe.rfind('/') {
            alloc::format!("({})", &exe[pos + 1..])
        } else {
            alloc::format!("({})", exe)
        }
    };

    // Linux /proc/<pid>/stat 单行空格分隔格式:
    // pid comm state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt
    // utime stime cutime cstime priority nice num_threads itrealvalue starttime vsize rss ...
    let s = alloc::format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
        pid,                                    // 1:  pid
        comm,                                   // 2:  comm (括号包裹)
        state_char,                             // 3:  state
        ppid,                                   // 4:  ppid
        pgid,                                   // 5:  pgrp
        0,                                      // 6:  session
        0,                                      // 7:  tty_nr
        -1,                                     // 8:  tpgid
        0,                                      // 9:  flags
        0,                                      // 10: minflt
        0,                                      // 11: cminflt
        0,                                      // 12: majflt
        0,                                      // 13: cmajflt
        0,                                      // 14: utime (not tracked)
        0,                                      // 15: stime (not tracked)
        0,                                      // 16: cutime (not tracked)
        0,                                      // 17: cstime (not tracked)
        20,                                     // 18: priority (default)
        0,                                      // 19: nice
        num_threads,                            // 20: num_threads
        0,                                      // 21: itrealvalue
        0,                                      // 22: starttime (not tracked)
        0,                                      // 23: vsize
        0,                                      // 24: rss
    );

    proc_read_str(offset, len, buf, &s)
}
