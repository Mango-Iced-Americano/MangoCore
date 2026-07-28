//! Regression: relative nanosleep blocks and reports remaining time when interrupted.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const CLOCK_MONOTONIC: usize = 1;
const CLOCK_SLEEP_NS: usize = 50_000_000;
const INTERRUPTED_SLEEP_NS: usize = 200_000_000;
const SIGNAL_DELAY_MS: usize = 50;
const MIN_BLOCK_MS: isize = 40;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGUSR1: usize = 10;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;
const EINTR: isize = -4;

extern "C" fn ignore_sigusr1(_signum: usize) {}

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_nanosleep] start");

    let request = TimeSpec { tv_sec: 0, tv_nsec: CLOCK_SLEEP_NS };
    let mut unused_remaining = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    let start = sys_get_time();
    let slept = sys_clock_nanosleep(CLOCK_MONOTONIC, 0, &request, &mut unused_remaining);
    let elapsed = sys_get_time() - start;
    if slept != 0 || elapsed < MIN_BLOCK_MS {
        println!("FAIL: clock_nanosleep={} elapsed={}ms", slept, elapsed);
        return 1;
    }

    let action = user_lib::SigAction {
        handler: ignore_sigusr1 as usize,
        flags: 0,
        restorer: 0,
        mask: 0,
    };
    if user_lib::sigaction(SIGUSR1, &action) != 0 {
        println!("FAIL: sigaction SIGUSR1 failed");
        return 1;
    }

    let parent = sys_getpid();
    let sender = sys_fork();
    if sender == 0 {
        sleep(SIGNAL_DELAY_MS);
        sys_exit(if sys_kill(parent as usize, SIGUSR1) == 0 { 0 } else { 1 });
    }
    if sender < 0 {
        println!("FAIL: sender fork returned {}", sender);
        return 1;
    }

    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    if watchdog < 0 {
        println!("FAIL: watchdog fork returned {}", watchdog);
        let _ = sys_kill(sender as usize, SIGKILL);
        let _ = reap(sender);
        return 1;
    }

    let interrupted_request = TimeSpec { tv_sec: 0, tv_nsec: INTERRUPTED_SLEEP_NS };
    let mut remaining = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    let interrupted = sys_nanosleep(&interrupted_request, &mut remaining);
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let sender_ok = reap(sender);
    let _ = reap(watchdog);

    if interrupted != EINTR
        || remaining.tv_sec != 0
        || remaining.tv_nsec == 0
        || remaining.tv_nsec >= INTERRUPTED_SLEEP_NS
        || !sender_ok
    {
        println!(
            "FAIL: nanosleep={} remaining={}ns sender_ok={}",
            interrupted, remaining.tv_nsec, sender_ok
        );
        return 1;
    }

    println!(
        "[regression_nanosleep] PASS: blocked {}ms, remaining {}ns",
        elapsed, remaining.tv_nsec
    );
    0
}
