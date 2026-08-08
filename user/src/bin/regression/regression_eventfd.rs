//! Regression: blocking eventfd reads require an explicit eventfd writer wakeup.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const WRITER_DELAY_MS: usize = 100;
const WATCHDOG_DELAY_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_eventfd] start");

    let fd = sys_eventfd(0, 0);
    if fd < 0 {
        println!("FAIL: eventfd returned {}", fd);
        return 1;
    }

    let writer = sys_fork();
    if writer == 0 {
        sleep(WRITER_DELAY_MS);
        let written = sys_write(fd as usize, &1u64.to_ne_bytes());
        sys_exit(if written == 8 { 0 } else { 1 });
    }
    if writer < 0 {
        println!("FAIL: writer fork returned {}", writer);
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
        let _ = sys_kill(writer as usize, SIGKILL);
        let _ = reap(writer);
        let _ = sys_close(fd as usize);
        return 1;
    }

    let mut counter = [0u8; 8];
    let first_read = sys_read(fd as usize, &mut counter);
    let first_value = u64::from_ne_bytes(counter);
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let writer_ok = reap(writer);
    let _ = reap(watchdog);
    if first_read != 8 || first_value != 1 || !writer_ok {
        println!("FAIL: blocking read={} value={} writer_ok={}", first_read, first_value, writer_ok);
        let _ = sys_close(fd as usize);
        return 1;
    }

    for expected in [3u64, 5] {
        if sys_write(fd as usize, &expected.to_ne_bytes()) != 8 {
            println!("FAIL: eventfd write {} failed", expected);
            let _ = sys_close(fd as usize);
            return 1;
        }
        counter = [0; 8];
        if sys_read(fd as usize, &mut counter) != 8 || u64::from_ne_bytes(counter) != expected {
            println!("FAIL: eventfd counter round trip {} failed", expected);
            let _ = sys_close(fd as usize);
            return 1;
        }
    }

    let _ = sys_close(fd as usize);
    println!("[regression_eventfd] PASS");
    0
}
