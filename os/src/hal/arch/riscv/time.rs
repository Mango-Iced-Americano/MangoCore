//! RISC-V 时间源和调度 tick 编程。
//!
//! 使用 SBI timer 设置下一次时钟中断，`get_time()` 读取硬件 time 寄存器。

use super::config::CLOCK_FREQ;
use crate::hal::arch::set_timer;
use riscv::register::time;
pub const TICKS_PER_SEC: usize = 25;

/// Return current time measured by ticks, which is NOT divided by frequency.
pub fn get_time() -> usize {
    time::read()
}

/// Set next trigger.
pub fn set_next_trigger() {
    set_timer(get_time() + CLOCK_FREQ / TICKS_PER_SEC);
}

/// Program a one-shot timer to fire after `delta_ticks` raw timer ticks.
#[inline]
pub fn program_timer_delta(delta_ticks: u64) {
    let profile_start = crate::task::processor::sched_profile_cycle_start();
    let now = get_time() as u64;
    set_timer(now.saturating_add(delta_ticks.max(1)) as usize);
    crate::task::processor::record_sched_program_timer_cycles(profile_start);
}

/// 清除当前 hart 的 timer pending，并在安全点处理前不再安排新事件。
///
/// SBI TIME 规定把比较值写到未来必须清除 pending bit；`usize::MAX` 在
/// RV64 上代表最远的绝对时间。安全点完成软件 timer 工作后会重新写入真实
/// deadline，因此 hard IRQ 不需要读取任何受锁队列。
pub fn quiesce_local_timer_interrupt() {
    set_timer(usize::MAX);
}

pub fn get_clock_freq() -> usize {
    CLOCK_FREQ
}
