use user_lib::println;
use user_lib::syscall::{sys_close, sys_get_time, sys_read, sys_timerfd_create, sys_timerfd_settime, TimeSpec, TimerFdSpec};
pub fn run_timerfd_smoke() -> bool {
    const CLOCK_MONOTONIC: usize = 1; println!("[timer_smoke] timerfd monotonic one-shot begin"); let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0); if fd < 0 { println!("[timer_smoke] timerfd_create failed ret={}", fd); return false; }
    let spec = TimerFdSpec { it_interval: TimeSpec { tv_sec: 0, tv_nsec: 0 }, it_value: TimeSpec { tv_sec: 0, tv_nsec: 2_000_000 } }; let start = sys_get_time(); if sys_timerfd_settime(fd as usize, 0, &spec, core::ptr::null_mut()) < 0 { let _ = sys_close(fd as usize); return false; } let mut buffer = [0; 8]; let count = sys_read(fd as usize, &mut buffer); let elapsed = sys_get_time().saturating_sub(start); let _ = sys_close(fd as usize); let passed = count == 8 && u64::from_ne_bytes(buffer) > 0 && elapsed <= 50; println!("[timer_smoke] {}", if passed { "PASS" } else { "result out of range" }); passed
}
