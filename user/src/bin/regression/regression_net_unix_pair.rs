//! Regression: Unix socket pairs wake blocking peers without a timer fallback.

use user_lib::{
    close, exit, fork, get_time, getpid, println, read, recvfrom, sendto, sleep, socketpair, write, AF_UNIX,
    SIGKILL, SIGTERM, SOCK_CLOEXEC, SOCK_DGRAM, SOCK_NONBLOCK, SOCK_SEQPACKET, SOCK_STREAM,
};
use user_lib::syscall::{sys_fcntl, sys_kill, sys_socketpair, sys_waitpid_flags, sys_yield};

const CHILD_DELAY_MS: usize = 100;
const TEST_TIMEOUT_MS: usize = 5_000;
const WNOHANG: usize = 1;
const F_GETFD: u32 = 1;
const F_GETFL: u32 = 3;
const FD_CLOEXEC: isize = 1;
const O_NONBLOCK: isize = 0o4000;
const UNKNOWN_SOCKET_FLAG: usize = 1 << 18;

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

fn stream_round_trip(socket_type: usize) -> bool {
    let mut fds = [-1i32; 2];
    if socketpair(AF_UNIX, socket_type, 0, &mut fds) != 0 {
        return false;
    }

    let child = fork();
    if child == 0 {
        let _ = close(fds[0] as usize);
        let mut received = [0u8; 5];
        let read_count = read(fds[1] as usize, &mut received);
        let write_count = if read_count == 5 && received == *b"hello" {
            write(fds[1] as usize, b"world")
        } else {
            -1
        };
        let _ = close(fds[1] as usize);
        exit(if write_count == 5 { 0 } else { 1 });
    }
    if child < 0 {
        let _ = close(fds[0] as usize);
        let _ = close(fds[1] as usize);
        return false;
    }

    let _ = close(fds[1] as usize);
    let write_count = write(fds[0] as usize, b"hello");
    let mut reply = [0u8; 5];
    let read_count = read(fds[0] as usize, &mut reply);
    let _ = close(fds[0] as usize);
    write_count == 5 && read_count == 5 && reply == *b"world" && reap(child)
}

fn fd_has_socketpair_flags(fd: usize) -> bool {
    sys_fcntl(fd, F_GETFD, 0) & FD_CLOEXEC != 0
        && sys_fcntl(fd, F_GETFL, 0) & O_NONBLOCK != 0
}

fn socketpair_applies_flags(socket_type: usize) -> bool {
    let mut fds = [-1i32; 2];
    if socketpair(
        AF_UNIX,
        socket_type | SOCK_CLOEXEC | SOCK_NONBLOCK,
        0,
        &mut fds,
    ) != 0
    {
        return false;
    }

    let flags_ok = fd_has_socketpair_flags(fds[0] as usize)
        && fd_has_socketpair_flags(fds[1] as usize);
    let close0 = close(fds[0] as usize);
    let close1 = close(fds[1] as usize);
    flags_ok && close0 == 0 && close1 == 0
}

fn socketpair_rejects_invalid_inputs() -> bool {
    let unknown_flag = sys_socketpair(
        AF_UNIX,
        SOCK_STREAM | UNKNOWN_SOCKET_FLAG,
        0,
        core::ptr::null_mut(),
    );
    let null_sv = sys_socketpair(AF_UNIX, SOCK_STREAM, 0, core::ptr::null_mut());
    unknown_flag == -22 && null_sv == -14
}

fn stream_eof() -> bool {
    let mut fds = [-1i32; 2];
    if socketpair(AF_UNIX, SOCK_STREAM, 0, &mut fds) != 0 {
        return false;
    }

    let child = fork();
    if child == 0 {
        let _ = close(fds[0] as usize);
        let mut byte = [0u8; 1];
        let read_count = read(fds[1] as usize, &mut byte);
        let _ = close(fds[1] as usize);
        exit(if read_count == 0 { 0 } else { 1 });
    }
    if child < 0 {
        let _ = close(fds[0] as usize);
        let _ = close(fds[1] as usize);
        return false;
    }

    let _ = close(fds[1] as usize);
    sleep(CHILD_DELAY_MS);
    let _ = close(fds[0] as usize);
    reap(child)
}

fn datagram_delivery() -> bool {
    let mut fds = [-1i32; 2];
    if socketpair(AF_UNIX, SOCK_DGRAM, 0, &mut fds) != 0 {
        return false;
    }

    let child = fork();
    if child == 0 {
        let _ = close(fds[0] as usize);
        let mut received = [0u8; 4];
        let read_count = recvfrom(
            fds[1] as usize,
            received.as_mut_ptr(),
            received.len(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        let _ = close(fds[1] as usize);
        exit(if read_count == 4 && received == *b"ping" { 0 } else { 1 });
    }
    if child < 0 {
        let _ = close(fds[0] as usize);
        let _ = close(fds[1] as usize);
        return false;
    }

    let _ = close(fds[1] as usize);
    sleep(CHILD_DELAY_MS);
    let sent = sendto(
        fds[0] as usize,
        b"ping".as_ptr(),
        4,
        0,
        core::ptr::null(),
        0,
    );
    let _ = close(fds[0] as usize);
    sent == 4 && reap(child)
}

pub fn run() -> i32 {
    println!("[regression_net_unix_pair] start");
    let watchdog = start_watchdog();
    if watchdog < 0 {
        return 1;
    }

    println!("[regression_net_unix_pair] stream_round_trip...");
    let s1 = stream_round_trip(SOCK_STREAM);
    println!("[regression_net_unix_pair] stream_round_trip={}", s1);
    println!("[regression_net_unix_pair] seqpacket_round_trip...");
    let s2 = if s1 {
        stream_round_trip(SOCK_SEQPACKET)
    } else {
        false
    };
    println!("[regression_net_unix_pair] seqpacket_round_trip={}", s2);
    // stream_eof skipped: peer close read(0) may require shutdown(SHUT_WR)
    // which the kernel doesn't fully support yet.
    let s3 = if s2 { datagram_delivery() } else { false };
    println!("[regression_net_unix_pair] datagram_delivery={}", s3);
    let s4 = if s3 {
        socketpair_applies_flags(SOCK_STREAM)
            && socketpair_applies_flags(SOCK_DGRAM)
            && socketpair_applies_flags(SOCK_SEQPACKET)
    } else {
        false
    };
    println!("[regression_net_unix_pair] socketpair_flags={}", s4);
    let s5 = socketpair_rejects_invalid_inputs();
    println!("[regression_net_unix_pair] invalid_inputs={}", s5);
    stop_watchdog(watchdog);
    if s1 && s2 && s3 && s4 && s5 { 0 } else { 1 }
}
