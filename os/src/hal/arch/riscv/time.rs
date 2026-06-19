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

pub fn get_clock_freq() -> usize {
    CLOCK_FREQ
}
