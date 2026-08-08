//! Regression: a TCP listener wakes a blocked accept when its peer connects.

use user_lib::{
    accept, bind, close, connect, exit, fork, get_time, getpid, getsockname, listen, read, sleep,
    socket, write, SIGKILL, SIGTERM, SOCK_STREAM,
};
use user_lib::syscall::{sys_kill, sys_waitpid_flags, sys_yield};

const AF_INET: usize = 2;
const CONNECT_DELAY_MS: usize = 100;
const MIN_BLOCK_MS: isize = 50;
const TEST_TIMEOUT_MS: usize = 5_000;
const WNOHANG: usize = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

fn loopback(port: u16) -> SockAddrIn {
    SockAddrIn {
        sin_family: AF_INET as u16,
        sin_port: port,
        sin_addr: [127, 0, 0, 1],
        sin_zero: [0; 8],
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

fn create_listener() -> Result<(usize, SockAddrIn), isize> {
    let listener = socket(AF_INET, SOCK_STREAM, 0);
    if listener < 0 {
        return Err(listener);
    }
    let mut address = loopback(0u16.to_be());
    let mut address_len = core::mem::size_of::<SockAddrIn>();
    let bound = bind(listener as usize, &address as *const SockAddrIn as *const u8, address_len);
    if bound < 0 {
        let _ = close(listener as usize);
        return Err(bound);
    }
    let named = getsockname(
        listener as usize,
        &mut address as *mut SockAddrIn as *mut u8,
        &mut address_len as *mut usize,
    );
    if named < 0 {
        let _ = close(listener as usize);
        return Err(named);
    }
    let listening = listen(listener as usize, 1);
    if listening < 0 {
        let _ = close(listener as usize);
        return Err(listening);
    }
    Ok((listener as usize, address))
}

pub fn run() -> i32 {
    let watchdog = start_watchdog();
    let result = (|| -> bool {
        if watchdog < 0 {
            return false;
        }
        let (listener, address) = match create_listener() {
            Ok(listener) => listener,
            Err(_) => return false,
        };
        let parent = getpid();
        let child = fork();
        if child == 0 {
            let _ = close(listener);
            if getpid() == parent {
                exit(1);
            }
            sleep(CONNECT_DELAY_MS);
            let client = socket(AF_INET, SOCK_STREAM, 0);
            if client < 0 {
                exit(1);
            }
            let connected = connect(
                client as usize,
                &address as *const SockAddrIn as *const u8,
                core::mem::size_of::<SockAddrIn>(),
            );
            let sent = if connected == 0 { write(client as usize, b"accept") } else { -1 };
            let _ = close(client as usize);
            exit(if sent == 6 { 0 } else { 1 });
        }
        if child < 0 {
            let _ = close(listener);
            return false;
        }

        let mut peer = loopback(0);
        let mut peer_len = core::mem::size_of::<SockAddrIn>();
        let start = get_time();
        let accepted = accept(
            listener,
            &mut peer as *mut SockAddrIn as *mut u8,
            &mut peer_len as *mut usize,
        );
        let elapsed = get_time() - start;
        let _ = close(listener);
        let mut message = [0u8; 6];
        let received = if accepted >= 0 { read(accepted as usize, &mut message) } else { -1 };
        if accepted >= 0 {
            let _ = close(accepted as usize);
        }
        accepted >= 0 && elapsed >= MIN_BLOCK_MS && received == 6 && message == *b"accept" && reap(child)
    })();
    stop_watchdog(watchdog);
    if result { 0 } else { 1 }
}
