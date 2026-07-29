//! Regression: waitpid blocks until a delayed child exit is published.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const CHILD_EXIT_DELAY_MS: usize = 100;
const MIN_BLOCK_MS: isize = 40;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

fn start_watchdog() -> isize {
    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    watchdog
}

fn stop_watchdog(watchdog: isize) {
    if watchdog > 0 {
        let _ = sys_kill(watchdog as usize, SIGKILL);
        let _ = reap(watchdog);
    }
}

pub fn run() -> i32 {
    println!("[regression_child_wait] start");
    let watchdog = start_watchdog();
    let result = (|| -> bool {
        if watchdog < 0 {
            return false;
        }

        let child = sys_fork();
        if child == 0 {
            sleep(CHILD_EXIT_DELAY_MS);
            sys_exit(0);
        }
        if child < 0 {
            return false;
        }

        let mut status = -1;
        let start = sys_get_time();
        let waited = sys_waitpid(child, &mut status);
        let elapsed = sys_get_time() - start;
        waited == child && status == 0 && elapsed >= MIN_BLOCK_MS
    })();
    stop_watchdog(watchdog);

    if result {
        println!("[regression_child_wait] PASS");
        0
    } else {
        println!("[regression_child_wait] FAIL");
        1
    }
}
