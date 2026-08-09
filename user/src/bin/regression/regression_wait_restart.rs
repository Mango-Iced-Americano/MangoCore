//! Regression: SA_RESTART controls whether SIGUSR1 restarts waitpid.

use core::sync::atomic::{AtomicUsize, Ordering};
use user_lib::{println, sleep, SigAction};
use user_lib::syscall::*;

const SIGUSR1: usize = 10;
const EINTR: isize = -4;
const SA_RESTART: usize = 0x1000_0000;
const SIGNAL_DELAY_MS: usize = 50;
const CHILD_EXIT_DELAY_MS: usize = 200;

static HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_sigusr1(_signum: usize) {
    HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
}

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

fn run_case(flags: usize, expect_restart: bool) -> bool {
    HANDLER_CALLS.store(0, Ordering::Relaxed);
    let action = SigAction {
        handler: count_sigusr1 as usize,
        flags,
        restorer: 0,
        mask: 0,
    };
    if user_lib::sigaction(SIGUSR1, &action) != 0 {
        return false;
    }

    let parent = sys_getpid();
    let child = sys_fork();
    if child == 0 {
        sleep(CHILD_EXIT_DELAY_MS);
        sys_exit(0);
    }
    if child < 0 {
        return false;
    }

    let sender = sys_fork();
    if sender == 0 {
        sleep(SIGNAL_DELAY_MS);
        sys_exit(if sys_kill(parent as usize, SIGUSR1) == 0 { 0 } else { 1 });
    }
    if sender < 0 {
        let _ = sys_kill(child as usize, 9);
        let _ = reap(child);
        return false;
    }

    let mut status = -1;
    let first_wait = sys_waitpid(child, &mut status);
    let child_reaped = if first_wait == child {
        status == 0
    } else {
        reap(child)
    };
    let sender_reaped = reap(sender);
    let handler_called = HANDLER_CALLS.load(Ordering::Relaxed) > 0;

    let wait_result = if expect_restart {
        first_wait == child && status == 0
    } else {
        first_wait == EINTR && child_reaped
    };
    handler_called && wait_result && sender_reaped
}

pub fn run() -> i32 {
    println!("[regression_wait_restart] start");
    let restart_ok = run_case(SA_RESTART, true);
    let eintr_ok = run_case(0, false);

    if restart_ok && eintr_ok {
        println!("[regression_wait_restart] PASS: SA_RESTART restarts, default returns EINTR");
        0
    } else {
        println!(
            "[regression_wait_restart] FAIL: restart_ok={} eintr_ok={}",
            restart_ok, eintr_ok
        );
        1
    }
}
