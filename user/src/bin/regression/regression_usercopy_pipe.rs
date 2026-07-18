//! Regression: failed copy to/from user consumed pipe data
//! Bug: When read from pipe fails due to bad user buffer (EFAULT),
//!      the pipe data was already consumed (lost).
//! Expected: Read with bad buffer returns -EFAULT, pipe data
//!           still available on next valid read.
//! Related subsystem: pipe / usercopy
//! LTP counterpart: none (found by Oracle audit)
//! Source: docs/02_syscall/fs-fd-event.md (§5.1 read/write user buffer)

use user_lib::syscall::*;
use user_lib::println;

const EFAULT: isize = -14;

pub fn run() -> i32 {
    println!("[regression_usercopy_pipe] start");

    // 1. Create pipe
    let mut pipefd: [i32; 2] = [-1, -1];
    let ret = sys_pipe(&mut pipefd);
    if ret < 0 {
        println!("FAIL: pipe() returned {}", ret);
        return 1;
    }
    let (rfd, wfd) = (pipefd[0] as usize, pipefd[1] as usize);
    println!("  pipe created: rfd={} wfd={}", rfd, wfd);

    // 2. Write known data to pipe
    let sent: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22];
    let n = sys_write(wfd, &sent);
    if n != 8 {
        println!("FAIL: write returned {} (expected 8)", n);
        return 1;
    }
    println!("  wrote {} bytes to pipe", n);

    // 3. Read with NULL buffer → should get -EFAULT, data NOT consumed
    let bad_ret = sys_read_raw(rfd, core::ptr::null_mut(), 8);
    println!("  read(null, 8) returned {} (expect {})", bad_ret, EFAULT);
    if bad_ret != EFAULT {
        println!("FAIL: unexpected return value from bad-buffer read");
        return 1;
    }

    // 4. Read again with valid buffer — should get the same data (not lost)
    let mut recv: [u8; 8] = [0; 8];
    let n2 = sys_read(rfd, &mut recv);
    if n2 != 8 {
        println!("FAIL: second read returned {} (expected 8)", n2);
        if n2 == 0 {
            println!("  → DATA LOST! Bug reproduced: pipe data consumed by failed read.");
        }
        return 1;
    }
    println!("  second read returned {} bytes", n2);

    // Verify data matches
    if recv != sent {
        println!("FAIL: data mismatch sent={:?} recv={:?}", sent, recv);
        return 1;
    }

    let _ = sys_close(rfd);
    let _ = sys_close(wfd);

    println!("[regression_usercopy_pipe] PASS");
    0
}
