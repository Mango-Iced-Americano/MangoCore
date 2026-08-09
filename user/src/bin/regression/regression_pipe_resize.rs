//! Regression: pipe capacity resize wakeup.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const F_SETPIPE_SZ: u32 = 1031;
const F_GETPIPE_SZ: u32 = 1032;
const SMALL: usize = 4096;
const LARGE: usize = 65536;
const BLOCK_OBSERVATION_MS: usize = 100;
const WATCHDOG_MS: usize = 5_000;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;
const WNOHANG: usize = 1;
const FILL_BYTE: u8 = 0xA5;
const WRITER_BYTE: u8 = 0x5A;

fn start_watchdog() -> isize {
    let parent = sys_getpid();
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_MS);
        let _ = sys_kill(parent as usize, SIGTERM);
        sys_exit(1);
    }
    watchdog
}

fn reap(pid: isize) -> bool {
    let mut status = -1;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

fn stop_watchdog(watchdog: isize) {
    if watchdog > 0 {
        let _ = sys_kill(watchdog as usize, SIGKILL);
        let mut status = 0;
        let _ = sys_waitpid(watchdog, &mut status);
    }
}

pub fn run() -> i32 {
    println!("[regression_pipe_resize] start");
    let watchdog = start_watchdog();
    if watchdog < 0 {
        println!("FAIL: watchdog fork returned {}", watchdog);
        return 1;
    }

    let mut data_pipe = [-1i32; 2];
    let mut ready_pipe = [-1i32; 2];
    let mut writer = 0isize;
    let mut writer_reaped = false;
    let result = (|| -> bool {
        if sys_pipe(&mut data_pipe) < 0 || sys_pipe(&mut ready_pipe) < 0 {
            println!("FAIL: pipe setup failed");
            return false;
        }
        let data_read = data_pipe[0] as usize;
        let data_write = data_pipe[1] as usize;
        let ready_read = ready_pipe[0] as usize;
        let ready_write = ready_pipe[1] as usize;

        let small = sys_fcntl(data_write, F_SETPIPE_SZ, SMALL);
        if small != SMALL as isize || sys_fcntl(data_write, F_GETPIPE_SZ, 0) != small {
            println!("FAIL: failed to set small capacity: {}", small);
            return false;
        }
        let fill = [FILL_BYTE; SMALL];
        if sys_write(data_write, &fill) != SMALL as isize {
            println!("FAIL: failed to fill {}-byte pipe", SMALL);
            return false;
        }
        println!("  pipe full at {} bytes", SMALL);

        writer = sys_fork();
        if writer < 0 {
            println!("FAIL: writer fork returned {}", writer);
            return false;
        }
        if writer == 0 {
            let _ = sys_close(data_read);
            let _ = sys_close(ready_read);
            let armed = sys_write(ready_write, &[1]);
            let wrote = if armed == 1 {
                sys_write(data_write, &[WRITER_BYTE])
            } else {
                -1
            };
            let _ = sys_close(ready_write);
            let _ = sys_close(data_write);
            sys_exit(if wrote == 1 { 0 } else { 1 });
        }

        let _ = sys_close(ready_write);
        let mut armed = [0u8; 1];
        if sys_read(ready_read, &mut armed) != 1 || armed != [1] {
            println!("FAIL: writer did not arm blocking write");
            return false;
        }
        println!("  writer armed; verifying it blocks");

        // The child has announced the next syscall and receives a full pipe.
        // Yielding here requires that write to block before resize; completing
        // early is a failure rather than an alternative passing path.
        sleep(BLOCK_OBSERVATION_MS);
        let mut status = -1;
        let pending = sys_waitpid_flags(writer, &mut status, WNOHANG);
        if pending == writer {
            writer_reaped = true;
        }
        if pending != 0 {
            println!("FAIL: writer completed before resize: wait={} status={}", pending, status);
            return false;
        }

        println!("  writer remains blocked; growing pipe to {} bytes", LARGE);
        let large = sys_fcntl(data_write, F_SETPIPE_SZ, LARGE);
        if large != LARGE as isize || sys_fcntl(data_write, F_GETPIPE_SZ, 0) != large {
            println!("FAIL: failed to grow capacity: {}", large);
            return false;
        }
        if !reap(writer) {
            println!("FAIL: writer did not complete after resize");
            return false;
        }
        writer_reaped = true;
        println!("  writer completed after resize; verifying data");

        let _ = sys_close(data_write);
        let mut received = [0u8; SMALL + 1];
        if sys_read(data_read, &mut received) != received.len() as isize
            || received[..SMALL] != fill
            || received[SMALL] != WRITER_BYTE
        {
            println!("FAIL: pipe contents did not include the woken writer byte");
            return false;
        }
        true
    })();

    if !result && writer > 0 && !writer_reaped {
        let _ = sys_kill(writer as usize, SIGKILL);
        let mut status = 0;
        let _ = sys_waitpid(writer, &mut status);
    }
    for fd in [data_pipe[0], data_pipe[1], ready_pipe[0], ready_pipe[1]] {
        if fd >= 0 {
            let _ = sys_close(fd as usize);
        }
    }
    stop_watchdog(watchdog);

    if result {
        println!("[regression_pipe_resize] PASS");
        0
    } else {
        println!("[regression_pipe_resize] FAIL");
        1
    }
}
