//! /proc/stat — 系统统计信息

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;
use core::fmt::Write;

pub fn stat_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let cpu_count = crate::smp::configured_cpu_count();
    let mut s = String::with_capacity(512 + cpu_count.saturating_mul(32));

    // Linux 先输出全局汇总行，再输出与逻辑 CPU 编号一致的 cpuN 行。
    // 时间记账尚未接入 procfs，因此十个字段继续显式为 0；这里仅修复拓扑，
    // 不能用 timer IRQ 或调度次数冒充 USER_HZ CPU 时间。
    let _ = write!(s, "cpu  0 0 0 0 0 0 0 0 0 0\n");
    for cpu in 0..cpu_count {
        let _ = writeln!(s, "cpu{} 0 0 0 0 0 0 0 0 0 0", cpu);
    }

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
