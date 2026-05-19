//! /proc/<pid>/status — 进程状态

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::{String, ToString};

pub fn pid_status_content(
    pid: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let task = match crate::task::find_task_by_pid(pid) {
        Some(t) => t,
        None => return Err(SyscallErr::ENOENT),
    };

    // Snapshot fields under inner lock, then release before format!
    let (state_char, ppid, sig_blk, sig_pnd, tgid) = {
        let inner = task.acquire_inner_lock();
        let state = match inner.task_status {
            crate::task::TaskStatus::Ready => "R (running)",
            crate::task::TaskStatus::Running => "R (running)",
            crate::task::TaskStatus::Interruptible => "S (sleeping)",
            crate::task::TaskStatus::Zombie => "Z (zombie)",
        };
        let ppid_str = inner
            .parent
            .as_ref()
            .and_then(|p| p.upgrade())
            .map(|p| p.pid.0.to_string())
            .unwrap_or_else(|| String::from("0"));
        let blk = inner.sigmask.bits();
        let pnd = inner.sigpending.bits();
        let tg = task.tgid;
        (state, ppid_str, blk, pnd, tg)
    };

    let proc_name = {
        let exe = task.exe_path.lock();
        if exe.is_empty() {
            String::from("initproc")
        } else if let Some(pos) = exe.rfind('/') {
            exe[pos + 1..].to_string()
        } else {
            exe.clone()
        }
    };

    let s = alloc::format!(
        "Name:\t{}\n\
         State:\t{}\n\
         Tgid:\t{}\n\
         Pid:\t{}\n\
         PPid:\t{}\n\
         Uid:\t0\t0\t0\t0\n\
         Gid:\t0\t0\t0\t0\n\
         FDSize:\t256\n\
         VmSize:\t       0 kB\n\
         VmRSS:\t        0 kB\n\
         VmData:\t       0 kB\n\
         Threads:\t1\n\
         SigQ:\t0/0\n\
         SigPnd:\t{:016x}\n\
         ShdPnd:\t0000000000000000\n\
         SigBlk:\t{:016x}\n\
         SigIgn:\t0000000000000000\n\
         SigCgt:\t0000000000000000\n\
         CapInh:\t0000000000000000\n\
         CapPrm:\t0000000000000000\n\
         CapEff:\t0000000000000000\n\
         CapBnd:\t0000000000000000\n\
         Seccomp:\t0\n",
        proc_name,
        state_char,
        tgid,
        pid,
        ppid,
        sig_pnd,
        sig_blk,
    );

    proc_read_str(offset, len, buf, &s)
}
