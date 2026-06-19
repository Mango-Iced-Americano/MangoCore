use core::arch::asm;

use crate::config;

pub const TICKS_PER_SEC: usize = 100;

/// Return current time measured by ticks, which is NOT divided by frequency.
pub fn get_time() -> usize {
    let mut counter: usize;
    unsafe {
        asm!(
        "rdtime.d {},{}",
        out(reg)counter,
        out(reg)_,
        );
    }
    counter
}

/// Program a one-shot timer to fire after `delta_ticks` timer counter ticks.
/// The hardware timer counts down at CLOCK_FREQ / 4, so delta_ticks is in
/// those units.  HW requires init_val to be a multiple of 4.
#[inline]
pub fn program_timer_delta(delta_ticks: u64) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    use super::register::TCfg;
    let val = (delta_ticks.max(1).saturating_add(3) & !3).max(4) as usize;
    let mut cfg = TCfg::read();
    cfg.set_enable(true)
        .set_periodic(false)
        .set_init_val(val);
    cfg.write();
    crate::task::processor::record_sched_program_timer_cycles(profile_start);
}

#[inline(always)]
pub fn get_clock_freq() -> usize {
    unsafe { super::config::CLOCK_FREQ }
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
    println!(
        "[get_timer_freq_first_time] clk freq: {}(from CPUCFG)",
        cc_freq
    );
    unsafe { super::config::CLOCK_FREQ = cc_freq as usize }
}
