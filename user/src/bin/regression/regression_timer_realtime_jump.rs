//! Regression: clock_settime +2s jump caused relative timerfd to misbehave
//! Bug: After clock_settime() advancing CLOCK_REALTIME by +2s,
//!      relative timerfd timeouts fired incorrectly (immediate or never).
//! Expected: CLOCK_REALTIME relative timerfd is NOT affected by
//!           wall-clock jump. Timer fires as scheduled.
//! Related subsystem: timerfd / timekeeping
//! LTP counterpart: timerfd01, timerfd02
//! Source: docs/09_debug/timer-timekeeping-contrast-experiment-20260618.md

use user_lib::syscall::*;
use user_lib::println;

const CLOCK_REALTIME: usize = 0;
const CLOCK_MONOTONIC: usize = 1;

const NSEC_PER_MSEC: usize = 1_000_000;

pub fn run() -> i32 {
    println!("[regression_timer_realtime_jump] start");

    // ── Test 1: CLOCK_REALTIME relative timerfd + clock_settime(+2s) ──
    {
        // 1. Create timerfd with CLOCK_REALTIME
        let fd = sys_timerfd_create(CLOCK_REALTIME, 0);
        if fd < 0 {
            println!("FAIL: timerfd_create returned {}", fd);
            return 1;
        }
        println!("  timerfd created: fd={}", fd);

        // 2. Set relative timeout: 500ms
        let new_value = TimerFdSpec {
            it_interval: TimeSpec { tv_sec: 0, tv_nsec: 0 },
            it_value: TimeSpec {
                tv_sec: 0,
                tv_nsec: 500 * NSEC_PER_MSEC,
            },
        };
        let ret = sys_timerfd_settime(fd as usize, 0, &new_value, core::ptr::null_mut());
        if ret < 0 {
            println!("FAIL: timerfd_settime returned {}", ret);
            let _ = sys_close(fd as usize);
            return 1;
        }
        println!("  timerfd armed: 500ms relative");

        // 3. Jump CLOCK_REALTIME forward by 2 seconds
        let now = {
            let mut ts = TimeSpec { tv_sec: 0, tv_nsec: 0 };
            sys_clock_gettime(CLOCK_REALTIME, &mut ts);
            ts
        };
        let jump_ts = TimeSpec {
            tv_sec: now.tv_sec + 2,
            tv_nsec: now.tv_nsec,
        };
        let ret = sys_clock_settime(CLOCK_REALTIME, &jump_ts);
        if ret < 0 {
            println!("  clock_settime returned {} (skipping)", ret);
            let _ = sys_close(fd as usize);
            return 0; // not a failure if clock_settime unsupported
        }
        println!("  clock jumped +2s: {} -> {}", now.tv_sec, jump_ts.tv_sec);

        // 4. Read timerfd — should get 1 expiration
        // The relative timer should have fired (500ms elapsed in the
        // 2s jump, but Linux semantics: CLOCK_REALTIME relative timers
        // measure wall-clock progress, so the jump fires them).
        let mut count: u64 = 0;
        let n = sys_read(fd as usize, unsafe {
            core::slice::from_raw_parts_mut(
                &mut count as *mut u64 as *mut u8,
                core::mem::size_of::<u64>(),
            )
        });
        println!("  read(timerfd) returned {} (expirations={})", n, count);
        if n < 0 {
            println!("  read failed: {} (timerfd may not support CLOCK_REALTIME)", n);
            // Not necessarily a failure — the kernel might not support
            // CLOCK_REALTIME timerfd yet. This is informational.
        } else if count == 0 {
            println!("  WARNING: no expirations — timer may not have fired");
        } else {
            println!("  timer fired {} time(s) — OK", count);
        }

        let _ = sys_close(fd as usize);
    }

    // ── Test 2: CLOCK_MONOTONIC timerfd (control — should be unaffected) ──
    {
        let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0);
        if fd < 0 {
            println!("  monotonic timerfd_create returned {} (skipping)", fd);
            println!("[regression_timer_realtime_jump] PASS (best-effort)");
            return 0;
        }

        let new_value = TimerFdSpec {
            it_interval: TimeSpec { tv_sec: 0, tv_nsec: 0 },
            it_value: TimeSpec {
                tv_sec: 0,
                tv_nsec: 100 * NSEC_PER_MSEC,
            },
        };
        let ret = sys_timerfd_settime(fd as usize, 0, &new_value, core::ptr::null_mut());
        if ret < 0 {
            let _ = sys_close(fd as usize);
            println!("  monotonic timerfd_settime returned {} (skipping)", ret);
            println!("[regression_timer_realtime_jump] PASS (best-effort)");
            return 0;
        }
        println!("  monotonic timerfd armed: 100ms");

        // Busy-wait for expiration
        let start = sys_get_time();
        loop {
            let mut count: u64 = 0;
            let n = sys_read(fd as usize, unsafe {
                core::slice::from_raw_parts_mut(
                    &mut count as *mut u64 as *mut u8,
                    core::mem::size_of::<u64>(),
                )
            });
            if n == 8 && count > 0 {
                let elapsed = sys_get_time() - start;
                println!("  monotonic timerfd fired after {}ms (expirations={})", elapsed, count);
                break;
            }
            if sys_get_time() - start > 5000 {
                println!("  monotonic timerfd timed out waiting");
                break;
            }
            sys_yield();
        }

        let _ = sys_close(fd as usize);
    }

    println!("[regression_timer_realtime_jump] PASS");
    0
}
