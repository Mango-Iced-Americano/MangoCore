//! /proc/<pid>/stat — 进程状态（Linux procfs 兼容格式）
//!
//! 仿照 DragonOS /proc/<pid>/stat 设计。

use crate::fs::procfs::proc_read_str;
use crate::hal::TICKS_PER_SEC;
use crate::task::TaskStatus;
use crate::timer::get_time_ms;
use crate::utils::error::SyscallErr;
use alloc::string::String;

fn format_stat_line(
    stat_pid: usize,
    process: &crate::task::ProcessControlBlock,
    state_char: char,
) -> String {
    let ppid = process.parent_pid();
    let pgid = process.getpgid();
    let num_threads = process.live_thread_count().max(1);
    let uptime_ticks = get_time_ms().saturating_mul(TICKS_PER_SEC) / 1000;

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

    // Linux /proc/<pid>/stat 单行空格分隔格式。字段数量需要保持完整，
    // glibc/musl 的 CPU clock fallback 会解析后段字段。
    // pid comm state ppid pgrp session tty_nr tpgid flags minflt cminflt majflt cmajflt
    // utime stime cutime cstime priority nice num_threads itrealvalue starttime vsize rss ...
    alloc::format!(
        "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n",
        stat_pid,                               // 1:  pid
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
        uptime_ticks,                           // 14: utime (rough fallback ticks)
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
        0,                                      // 25: rsslim
        0,                                      // 26: startcode
        0,                                      // 27: endcode
        0,                                      // 28: startstack
        0,                                      // 29: kstkesp
        0,                                      // 30: kstkeip
        0,                                      // 31: signal
        0,                                      // 32: blocked
        0,                                      // 33: sigignore
        0,                                      // 34: sigcatch
        0,                                      // 35: wchan
        0,                                      // 36: nswap
        0,                                      // 37: cnswap
        17,                                     // 38: exit_signal (SIGCHLD)
        0,                                      // 39: processor
        0,                                      // 40: rt_priority
        0,                                      // 41: policy
        0,                                      // 42: delayacct_blkio_ticks
        0,                                      // 43: guest_time
        0,                                      // 44: cguest_time
        0,                                      // 45: start_data
        0,                                      // 46: end_data
        0,                                      // 47: start_brk
        0,                                      // 48: arg_start
        0,                                      // 49: arg_end
        0,                                      // 50: env_start
        0,                                      // 51: env_end
        0,                                      // 52: exit_code
    )
}

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

    let s = format_stat_line(pid, &process, state_char);

    proc_read_str(offset, len, buf, &s)
}

pub fn task_stat_content(
    extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let pid = extra >> 32;
    let tid = extra & 0xffff_ffff;
    let process = match crate::task::find_process_by_pid(pid) {
        Some(process) => process,
        None => return Err(SyscallErr::ENOENT),
    };
    let task = match crate::task::find_task_by_pid_tid(pid, tid) {
        Some(task) => task,
        None => return Err(SyscallErr::ENOENT),
    };
    let state_char = {
        let inner = task.acquire_inner_lock();
        match inner.task_status {
            TaskStatus::Ready => 'R',
            TaskStatus::Running => 'R',
            TaskStatus::Interruptible => 'S',
            TaskStatus::Zombie => 'Z',
        }
    };
    let s = format_stat_line(tid, &process, state_char);
    proc_read_str(offset, len, buf, &s)
}
