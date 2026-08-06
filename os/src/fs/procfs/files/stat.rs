//! /proc/stat — 系统统计信息

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;
use core::fmt::Write;

/// Linux `/proc/stat` CPU 时间使用 USER_HZ=100，而内部账户使用微秒。
const USEC_PER_USER_HZ_TICK: u64 = 10_000;

fn cpu_ticks(cpu: usize) -> (u64, u64, u64) {
    let time = crate::task::processor::cpu_time_snapshot(cpu);
    (
        time.user_us / USEC_PER_USER_HZ_TICK,
        time.system_us / USEC_PER_USER_HZ_TICK,
        time.idle_us / USEC_PER_USER_HZ_TICK,
    )
}

pub fn stat_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let cpu_count = crate::smp::configured_cpu_count();
    let mut s = String::with_capacity(512 + cpu_count.saturating_mul(32));

    // Linux 先输出全局汇总行，再输出与逻辑 CPU 编号一致的 cpuN 行。
    // MangoCore 目前精确区分 user/system/idle；nice、iowait、irq、softirq、
    // steal、guest 尚无独立账户，因此明确保留为 0，不能用中断次数冒充时间。
    let mut total_user = 0u64;
    let mut total_system = 0u64;
    let mut total_idle = 0u64;
    let mut per_cpu = alloc::vec::Vec::with_capacity(cpu_count);
    for cpu in 0..cpu_count {
        let ticks = cpu_ticks(cpu);
        total_user = total_user.saturating_add(ticks.0);
        total_system = total_system.saturating_add(ticks.1);
        total_idle = total_idle.saturating_add(ticks.2);
        per_cpu.push(ticks);
    }
    let _ = writeln!(
        s,
        "cpu  {} 0 {} {} 0 0 0 0 0 0",
        total_user, total_system, total_idle
    );
    for (cpu, (user, system, idle)) in per_cpu.into_iter().enumerate() {
        let _ = writeln!(
            s,
            "cpu{} {} 0 {} {} 0 0 0 0 0 0",
            cpu, user, system, idle
        );
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
