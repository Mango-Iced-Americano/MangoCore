//! Regression: pidfd poll readiness survives process exit and WNOWAIT observation.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const POLLIN: u16 = 0x001;
const WEXITED: u32 = 0x0000_0004;
const WNOHANG: u32 = 0x0000_0001;
const WNOWAIT: u32 = 0x0100_0000;
const P_PID: usize = 1;
const EXIT_TIMEOUT_MS: isize = 3_000;
const CHILD_DELAY_MS: usize = 100;

#[repr(C, align(8))]
struct SigInfoBytes([u8; 128]);

fn wait_for_pollin(fd: usize) -> bool {
    let timeout = TimeSpec {
        tv_sec: (EXIT_TIMEOUT_MS / 1_000) as usize,
        tv_nsec: ((EXIT_TIMEOUT_MS % 1_000) * 1_000_000) as usize,
    };
    let mut pollfd = [PollFd {
        fd: fd as u32,
        events: POLLIN,
        revents: 0,
    }];
    sys_ppoll(&mut pollfd, &timeout) == 1 && pollfd[0].revents & POLLIN != 0
}

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

fn wait_for_exit_without_reap(pid: isize) -> bool {
    let deadline = sys_get_time() + EXIT_TIMEOUT_MS;
    while sys_get_time() < deadline {
        let mut info = SigInfoBytes([0; 128]);
        let ret = sys_waitid(
            P_PID,
            pid as usize,
            info.0.as_mut_ptr(),
            WEXITED | WNOHANG | WNOWAIT,
        );
        if ret < 0 {
            return false;
        }
        let signo = u32::from_ne_bytes([info.0[0], info.0[1], info.0[2], info.0[3]]);
        if signo != 0 {
            return true;
        }
        let _ = sys_yield();
    }
    false
}

pub fn run() -> i32 {
    println!("[regression_pidfd] start");

    let child = sys_fork();
    if child == 0 {
        sleep(CHILD_DELAY_MS);
        sys_exit(0);
    }
    if child < 0 {
        println!("FAIL: first fork returned {}", child);
        return 1;
    }
    let pidfd = sys_pidfd_open(child as usize, 0);
    if pidfd < 0 {
        println!("FAIL: pidfd_open(live child) returned {}", pidfd);
        let _ = reap(child);
        return 1;
    }
    let live_ready = wait_for_pollin(pidfd as usize);
    let live_reaped = reap(child);
    let _ = sys_close(pidfd as usize);
    if !live_ready || !live_reaped {
        println!("FAIL: live pidfd readiness={} reaped={}", live_ready, live_reaped);
        return 1;
    }

    let exited_child = sys_fork();
    if exited_child == 0 {
        sys_exit(0);
    }
    if exited_child < 0 {
        println!("FAIL: second fork returned {}", exited_child);
        return 1;
    }
    if !wait_for_exit_without_reap(exited_child) {
        println!("FAIL: child did not become zombie before timeout");
        let _ = reap(exited_child);
        return 1;
    }
    let exited_pidfd = sys_pidfd_open(exited_child as usize, 0);
    if exited_pidfd < 0 {
        println!("FAIL: pidfd_open(exited child) returned {}", exited_pidfd);
        let _ = reap(exited_child);
        return 1;
    }
    let exited_ready = wait_for_pollin(exited_pidfd as usize);
    let exited_reaped = reap(exited_child);
    let _ = sys_close(exited_pidfd as usize);
    if !exited_ready || !exited_reaped {
        println!("FAIL: exited pidfd readiness={} reaped={}", exited_ready, exited_reaped);
        return 1;
    }

    println!("[regression_pidfd] PASS");
    0
}
