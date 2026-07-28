//! Regression: FUTEX_CMP_REQUEUE moves a waiter to a second shared futex.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const PAGE_SIZE: usize = 4096;
const PROT_READ_WRITE: usize = 3;
const MAP_SHARED: usize = 1;
const MAP_ANONYMOUS: usize = 0x20;
const FUTEX_WAIT: u32 = 0;
const FUTEX_WAKE: u32 = 1;
const REQUEUE_TIMEOUT_MS: isize = 1_000;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_futex_requeue] start");

    let mapping = sys_mmap(0, PAGE_SIZE, PROT_READ_WRITE, MAP_SHARED | MAP_ANONYMOUS, 0, 0);
    if mapping < 0 {
        println!("FAIL: shared mmap returned {}", mapping);
        return 1;
    }
    let first = mapping as *mut u32;
    let second = (mapping as usize + core::mem::size_of::<u32>()) as *mut u32;

    let mut ready_pipe = [-1i32; 2];
    if sys_pipe(&mut ready_pipe) < 0 {
        println!("FAIL: pipe failed");
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
        return 1;
    }
    let waiter = sys_fork();
    if waiter == 0 {
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_write(ready_pipe[1] as usize, b"r");
        let _ = sys_close(ready_pipe[1] as usize);
        let waited = sys_futex(first, FUTEX_WAIT, 0, core::ptr::null(), core::ptr::null_mut(), 0);
        sys_exit(if waited == 0 { 0 } else { 1 });
    }
    if waiter < 0 {
        println!("FAIL: waiter fork returned {}", waiter);
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_close(ready_pipe[1] as usize);
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
        return 1;
    }
    let _ = sys_close(ready_pipe[1] as usize);
    let mut ready = [0u8; 1];
    if sys_read(ready_pipe[0] as usize, &mut ready) != 1 {
        println!("FAIL: waiter did not signal readiness");
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_kill(waiter as usize, SIGKILL);
        let _ = reap(waiter);
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
        return 1;
    }
    let _ = sys_close(ready_pipe[0] as usize);

    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    if watchdog < 0 {
        println!("FAIL: watchdog fork returned {}", watchdog);
        let _ = sys_kill(waiter as usize, SIGKILL);
        let _ = reap(waiter);
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
        return 1;
    }

    let deadline = sys_get_time() + REQUEUE_TIMEOUT_MS;
    let mut requeued = 0;
    while sys_get_time() < deadline && requeued == 0 {
        requeued = sys_futex_cmp_requeue(first, 0, 1, second, 0);
        if requeued == 0 {
            let _ = sys_yield();
        }
    }
    let woken = if requeued == 1 {
        sys_futex(second, FUTEX_WAKE, 1, core::ptr::null(), core::ptr::null_mut(), 0)
    } else {
        -1
    };
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let waiter_ok = reap(waiter);
    let _ = reap(watchdog);
    let _ = sys_munmap(mapping as usize, PAGE_SIZE);

    if requeued != 1 || woken != 1 || !waiter_ok {
        println!(
            "FAIL: requeued={} woken={} waiter_ok={}",
            requeued, woken, waiter_ok
        );
        return 1;
    }

    println!("[regression_futex_requeue] PASS");
    0
}
