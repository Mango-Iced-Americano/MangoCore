//! Regression: shared futex waits need an explicit FUTEX_WAKE producer wakeup.

use core::sync::atomic::{AtomicU32, Ordering};
use user_lib::{println, sleep};
use user_lib::syscall::*;

const PAGE_SIZE: usize = 4096;
const PROT_READ_WRITE: usize = 3;
const MAP_SHARED: usize = 1;
const MAP_ANONYMOUS: usize = 0x20;
const FUTEX_WAIT: u32 = 0;
const FUTEX_WAKE: u32 = 1;
const EAGAIN: isize = -11;
const WAKER_DELAY_MS: usize = 100;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_futex] start");

    let mapping = sys_mmap(0, PAGE_SIZE, PROT_READ_WRITE, MAP_SHARED | MAP_ANONYMOUS, 0, 0);
    if mapping < 0 {
        println!("FAIL: shared mmap returned {}", mapping);
        return 1;
    }
    // SAFETY: Categories 3, 6, and 11. mmap returned a live page-aligned mapping
    // with at least one AtomicU32 of storage; this test does not unmap it until all
    // atomic references have gone out of scope.
    let word = unsafe { AtomicU32::from_ptr(mapping as *mut u32) };
    word.store(0, Ordering::SeqCst);

    let waker = sys_fork();
    if waker == 0 {
        sleep(WAKER_DELAY_MS);
        // SAFETY: Categories 3, 6, and 11. The inherited MAP_SHARED mapping stays
        // live until this child exits and has the same page-aligned base address.
        let word = unsafe { AtomicU32::from_ptr(mapping as *mut u32) };
        word.store(1, Ordering::SeqCst);
        let woken = sys_futex(word.as_ptr(), FUTEX_WAKE, 1, core::ptr::null(), core::ptr::null_mut(), 0);
        sys_exit(if woken == 1 { 0 } else { 1 });
    }
    if waker < 0 {
        println!("FAIL: waker fork returned {}", waker);
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
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
        let _ = sys_kill(waker as usize, SIGKILL);
        let _ = reap(waker);
        let _ = sys_munmap(mapping as usize, PAGE_SIZE);
        return 1;
    }

    let waited = sys_futex(word.as_ptr(), FUTEX_WAIT, 0, core::ptr::null(), core::ptr::null_mut(), 0);
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let waker_ok = reap(waker);
    let _ = reap(watchdog);
    let immediate = sys_futex(word.as_ptr(), FUTEX_WAIT, 0, core::ptr::null(), core::ptr::null_mut(), 0);
    let final_value = word.load(Ordering::SeqCst);
    let _ = sys_munmap(mapping as usize, PAGE_SIZE);

    if waited != 0 || immediate != EAGAIN || final_value != 1 || !waker_ok {
        println!("FAIL: wait={} immediate={} value={} waker_ok={}", waited, immediate, final_value, waker_ok);
        return 1;
    }

    println!("[regression_futex] PASS");
    0
}
