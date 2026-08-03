//! LoongArch64 时间源和调度 tick 编程。
//!
//! 读取 stable counter，并通过 timer CSR 设置下一次时钟中断。

use core::{arch::asm, sync::atomic::Ordering};

use crate::config;

pub const TICKS_PER_SEC: usize = 100;

/// Return current time measured by ticks, which is NOT divided by frequency.
pub fn get_time() -> usize {
    let mut counter: usize;
    // Safety: `rdtime.d` only reads the architectural stable counter and writes
    // output registers.
    unsafe {
        asm!(
        "rdtime.d {},{}",
        out(reg)counter,
        out(reg)_,
        );
    }
    counter
}

/// Program a one-shot timer to fire after `delta_ticks` stable-counter ticks.
/// TCFG.InitVal requires the low two bits to be zero, so round up to a
/// multiple of four without changing the counter's frequency domain.
#[inline]
pub fn program_timer_delta(delta_ticks: u64) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    use super::register::TCfg;
    let val = (delta_ticks.max(1).saturating_add(3) & !3).max(4) as usize;
    let mut cfg = TCfg::read();
    cfg.set_enable(true).set_periodic(false).set_init_val(val);
    cfg.write();
    crate::task::processor::record_sched_program_timer_cycles(profile_start);
}

/// 停止当前 CPU 的 timer 计数，并清除 level-triggered 中断信号。
///
/// `TICLR` 只清 pending，不能取消尚未到期的倒计时。先清除 `TCFG.En` 才能
/// 保证调用返回后 timer 保持静默；安全点处理完软件队列后，再由
/// `program_timer_delta()` 写入下一个真实 deadline 并重新启用计数。
pub fn quiesce_local_timer_interrupt() {
    use super::register::{TCfg, TIClr};
    TCfg::read().set_enable(false).write();
    TIClr::read().clear_timer().write();
}

#[inline(always)]
pub fn get_clock_freq() -> usize {
    // CPU0 的 Release store 发生在 AP 启动 Release 之前；运行期不再改写。
    super::config::CLOCK_FREQ.load(Ordering::Acquire)
}
pub fn get_timer_freq_first_time() {
    // 获取时钟晶振频率
    // 配置信息字index:4
    let base_freq = config::CPUCfg4::read().get_bits(0, 31);
    // 获取时钟倍频因子
    // 配置信息字index:5 位:0-15
    let cfg5 = config::CPUCfg5::read();
    let mul = cfg5.get_bits(0, 15);
    let div = cfg5.get_bits(16, 31);
    // 计算时钟频率
    let cc_freq = base_freq * mul / div;
    boot_trace!(
        "[get_timer_freq_first_time] clk freq: {}(from CPUCFG)",
        cc_freq
    );
    // machine_init 只能由 CPU0 执行；Release 把频率发布给后续进入 timer_cpu_init 的 AP。
    super::config::CLOCK_FREQ.store(cc_freq as usize, Ordering::Release);
}
