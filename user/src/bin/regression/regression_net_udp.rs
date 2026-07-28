//! Regression: a UDP datagram wakes a blocked recvfrom without a timer fallback.

use user_lib::{
    bind, close, exit, fork, get_time, getpid, getsockname, recvfrom, sendto, sleep, socket,
    SIGKILL, SIGTERM, SOCK_DGRAM,
};
use user_lib::syscall::{sys_kill, sys_waitpid_flags, sys_yield};

const AF_INET: usize = 2;
const SEND_DELAY_MS: usize = 100;
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

fn create_receiver() -> Result<(usize, SockAddrIn), isize> {
    let receiver = socket(AF_INET, SOCK_DGRAM, 0);
    if receiver < 0 {
        return Err(receiver);
    }

    let mut address = loopback(0u16.to_be());
    let mut address_len = core::mem::size_of::<SockAddrIn>();
    let bound = bind(
        receiver as usize,
        &address as *const SockAddrIn as *const u8,
        address_len,
    );
    if bound < 0 {
        let _ = close(receiver as usize);
        return Err(bound);
    }

    let named = getsockname(
        receiver as usize,
        &mut address as *mut SockAddrIn as *mut u8,
        &mut address_len as *mut usize,
    );
    if named < 0 {
        let _ = close(receiver as usize);
        return Err(named);
    }
    Ok((receiver as usize, address))
}

pub fn run() -> i32 {
    let watchdog = start_watchdog();
    let result = (|| -> bool {
        if watchdog < 0 {
            return false;
        }
        let (receiver, address) = match create_receiver() {
            Ok(receiver) => receiver,
            Err(_) => return false,
        };

        let parent = getpid();
        let child = fork();
        if child == 0 {
            if getpid() == parent {
                exit(1);
            }
            let _ = close(receiver);
            sleep(SEND_DELAY_MS);
            let sender = socket(AF_INET, SOCK_DGRAM, 0);
            if sender < 0 {
                exit(1);
            }
            let sent = sendto(
                sender as usize,
                b"hello".as_ptr(),
                5,
                0,
                &address as *const SockAddrIn as *const u8,
                core::mem::size_of::<SockAddrIn>(),
            );
            let _ = close(sender as usize);
            exit(if sent == 5 { 0 } else { 1 });
        }
        if child < 0 {
            let _ = close(receiver);
            return false;
        }

        let mut received = [0u8; 5];
        let start = get_time();
        let count = recvfrom(
            receiver,
            received.as_mut_ptr(),
            received.len(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        let elapsed = get_time() - start;
        let _ = close(receiver);
        count == 5 && received == *b"hello" && elapsed >= MIN_BLOCK_MS && reap(child)
    })();
    stop_watchdog(watchdog);
    if result { 0 } else { 1 }
}
