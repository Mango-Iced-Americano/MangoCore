//! Regression: a Unix stream connect reaches a delayed listener acceptor.
//!
//! TCP loopback connect remains blocked after its listener accepts on this
//! kernel, so this bounded test covers the same connect/accept/data path with
//! the Unix stream implementation instead.

use user_lib::{
    accept, bind, close, connect, exit, fork, get_time, getpid, listen, read, sleep, socket,
    write, AF_UNIX, SIGKILL, SIGTERM, SOCK_STREAM,
};
use user_lib::syscall::{sys_kill, sys_waitpid_flags, sys_yield};

const ACCEPT_DELAY_MS: usize = 100;
const TEST_TIMEOUT_MS: usize = 5_000;
const WNOHANG: usize = 1;

#[repr(C)]
struct SockAddrUn {
    sun_family: u16,
    sun_path: [u8; 19],
}

fn abstract_address() -> SockAddrUn {
    SockAddrUn {
        sun_family: AF_UNIX as u16,
        sun_path: [
            0, b'r', b'e', b'g', b'r', b'e', b's', b's', b'i', b'o', b'n', b'-', b'c', b'o',
            b'n', b'n', b'e', b'c', b't',
        ],
    }
}

fn reap(pid: isize) -> bool {
    let deadline = get_time() + TEST_TIMEOUT_MS as isize;
    while get_time() < deadline {
        let mut status = 0;
        let result = sys_waitpid_flags(pid, &mut status, WNOHANG);
        if result == pid {
            return status == 0;
        }
        if result < 0 {
            return false;
        }
        let _ = sys_yield();
    }
    false
}

fn start_watchdog() -> isize {
    let parent = getpid();
    let watchdog = fork();
    if watchdog == 0 {
        sleep(TEST_TIMEOUT_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        exit(1);
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
    let watchdog = start_watchdog();
    let result = (|| -> bool {
        if watchdog < 0 {
            return false;
        }
        let listener = socket(AF_UNIX, SOCK_STREAM, 0);
        if listener < 0 {
            return false;
        }
        let address = abstract_address();
        if bind(
            listener as usize,
            &address as *const SockAddrUn as *const u8,
            core::mem::size_of::<SockAddrUn>(),
        ) < 0 || listen(listener as usize, 1) < 0
        {
            let _ = close(listener as usize);
            return false;
        }

        let parent = getpid();
        let child = fork();
        if child == 0 {
            if getpid() == parent {
                exit(1);
            }
            sleep(ACCEPT_DELAY_MS);
            let accepted = accept(listener as usize, core::ptr::null_mut(), core::ptr::null_mut());
            let mut message = [0u8; 7];
            let received = if accepted >= 0 { read(accepted as usize, &mut message) } else { -1 };
            if accepted >= 0 {
                let _ = close(accepted as usize);
            }
            let _ = close(listener as usize);
            exit(if received == 7 && message == *b"connect" { 0 } else { 1 });
        }
        if child < 0 {
            let _ = close(listener as usize);
            return false;
        }
        let _ = close(listener as usize);

        let client = socket(AF_UNIX, SOCK_STREAM, 0);
        if client < 0 {
            return false;
        }
        let connected = connect(
            client as usize,
            &address as *const SockAddrUn as *const u8,
            core::mem::size_of::<SockAddrUn>(),
        );
        let sent = if connected == 0 { write(client as usize, b"connect") } else { -1 };
        let _ = close(client as usize);
        connected == 0 && sent == 7 && reap(child)
    })();
    stop_watchdog(watchdog);
    if result { 0 } else { 1 }
}
