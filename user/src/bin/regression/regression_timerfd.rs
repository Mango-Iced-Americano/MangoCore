//! Regression: a blocking timerfd read wakes when its one-shot timer expires.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const CLOCK_MONOTONIC: usize = 1;
const TIMER_DELAY_NS: usize = 50_000_000;
const MIN_BLOCK_MS: isize = 20;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) {
    let mut status = 0;
    let _ = sys_waitpid(pid, &mut status);
}

pub fn run() -> i32 {
    println!("[regression_timerfd] start");

    let fd = sys_timerfd_create(CLOCK_MONOTONIC, 0);
    if fd < 0 {
        println!("FAIL: timerfd_create returned {}", fd);
        return 1;
    }
    let spec = TimerFdSpec {
        it_interval: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        it_value: TimeSpec { tv_sec: 0, tv_nsec: TIMER_DELAY_NS },
    };
    if sys_timerfd_settime(fd as usize, 0, &spec, core::ptr::null_mut()) < 0 {
        println!("FAIL: timerfd_settime failed");
        let _ = sys_close(fd as usize);
        return 1;
    }

    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    if watchdog < 0 {
        println!("FAIL: watchdog fork returned {}", watchdog);
        let _ = sys_close(fd as usize);
        return 1;
    }

    let start = sys_get_time();
    let mut expiration = [0u8; 8];
    let read_count = sys_read(fd as usize, &mut expiration);
    let elapsed = sys_get_time() - start;
    let _ = sys_kill(watchdog as usize, SIGKILL);
    reap(watchdog);
    let _ = sys_close(fd as usize);
    let count = u64::from_ne_bytes(expiration);

    if read_count != 8 || count == 0 || elapsed < MIN_BLOCK_MS {
        println!("FAIL: read={} expirations={} elapsed={}ms", read_count, count, elapsed);
        return 1;
    }

    println!("[regression_timerfd] PASS: blocked {}ms", elapsed);
    0
}
