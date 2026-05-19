//! /proc/stat — 系统统计信息

use crate::utils::error::SyscallErr;
use crate::fs::procfs::proc_read_str;
use alloc::string::String;
use core::fmt::Write;

pub fn stat_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = String::with_capacity(512);

    // CPU line: cpu user nice system idle iowait irq softirq steal guest guest_nice
    let _ = write!(s, "cpu  0 0 0 0 0 0 0 0 0 0\n");
    let _ = write!(s, "cpu0 0 0 0 0 0 0 0 0 0 0\n");

    // Interrupts / context switches (simplified)
    let _ = write!(s, "intr 0 0\n");
    let _ = write!(s, "ctxt 0\n");

    // Boot time
    let btime = crate::timer::current_time();
    let _ = write!(s, "btime {}\n", btime);

    // Process counts
    let processes = crate::task::procs_count();
    let (running, blocked) = crate::task::task_manager_counts().unwrap_or((0, 0));
    let _ = write!(s, "processes {}\n", processes);
    let _ = write!(s, "procs_running {}\n", running);
    let _ = write!(s, "procs_blocked {}\n", blocked);

    // Softirq (simplified)
    let _ = write!(s, "softirq 0 0 0 0 0 0 0 0 0 0 0\n");

    proc_read_str(offset, len, buf, &s)
}
