//! Regression: blocking signalfd reads wait for the process signal event queue.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const SIGUSR1: usize = 10;
const SIGTERM: usize = 15;
const SIGKILL: usize = 9;
const SIG_BLOCK: usize = 0;
const SIG_UNBLOCK: usize = 1;
const SEND_DELAY_MS: usize = 100;
const WATCHDOG_DELAY_MS: usize = 5_000;
const MIN_BLOCK_MS: isize = 50;

fn signal_mask(signum: usize) -> u64 {
    1u64 << (signum - 1)
}

fn reap(pid: isize) {
    let mut status = 0;
    let _ = sys_waitpid(pid, &mut status);
}

pub fn run() -> i32 {
    println!("[regression_signalfd] start");

    let mask = signal_mask(SIGUSR1);
    if sys_rt_sigprocmask(SIG_BLOCK, &mask, core::ptr::null_mut(), core::mem::size_of::<u64>()) < 0 {
        println!("FAIL: could not block SIGUSR1");
        return 1;
    }

    let fd = sys_signalfd4(-1, &mask, core::mem::size_of::<u64>(), 0);
    if fd < 0 {
        println!("FAIL: signalfd4 returned {}", fd);
        let _ = sys_rt_sigprocmask(SIG_UNBLOCK, &mask, core::ptr::null_mut(), core::mem::size_of::<u64>());
        return 1;
    }

    let parent = sys_getpid();
    let sender = sys_fork();
    if sender == 0 {
        sleep(SEND_DELAY_MS);
        let result = sys_kill(parent as usize, SIGUSR1);
        sys_exit(if result == 0 { 0 } else { 1 });
    }
    if sender < 0 {
        println!("FAIL: sender fork returned {}", sender);
        let _ = sys_close(fd as usize);
        let _ = sys_rt_sigprocmask(SIG_UNBLOCK, &mask, core::ptr::null_mut(), core::mem::size_of::<u64>());
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
        reap(sender);
        let _ = sys_close(fd as usize);
        let _ = sys_rt_sigprocmask(SIG_UNBLOCK, &mask, core::ptr::null_mut(), core::mem::size_of::<u64>());
        return 1;
    }

    let start = sys_get_time();
    let mut info = [0u8; 128];
    let read_count = sys_read(fd as usize, &mut info);
    let elapsed = sys_get_time() - start;

    let _ = sys_kill(watchdog as usize, SIGKILL);
    reap(sender);
    reap(watchdog);
    let _ = sys_close(fd as usize);
    let _ = sys_rt_sigprocmask(SIG_UNBLOCK, &mask, core::ptr::null_mut(), core::mem::size_of::<u64>());

    let signo = u32::from_ne_bytes([info[0], info[1], info[2], info[3]]);
    if read_count != 128 || signo != SIGUSR1 as u32 || elapsed < MIN_BLOCK_MS {
        println!(
            "FAIL: read={} signo={} elapsed={}ms",
            read_count, signo, elapsed
        );
        return 1;
    }

    println!("[regression_signalfd] PASS: blocked {}ms", elapsed);
    0
}
