//! Regression: signalfd waits on the current sighand, including after fork.

use user_lib::syscall::*;
use user_lib::{println, sleep};

const SIGUSR1: usize = 10;
const SIGTERM: usize = 15;
const SIGKILL: usize = 9;
const SIG_BLOCK: usize = 0;
const SIG_UNBLOCK: usize = 1;
const SEND_DELAY_MS: usize = 100;
const WATCHDOG_DELAY_MS: usize = 5_000;
const MIN_BLOCK_MS: isize = 50;

fn signal_mask(signum: usize) -> u64 {
    1u64 << (signum - 1)
}

fn reap(pid: isize) -> bool {
    // 本测例只要求回收辅助进程；使用 NULL 将 wait4 状态编码与用户
    // copyout 从 signalfd 的通过条件中隔离。
    sys_waitpid(pid, core::ptr::null_mut()) == pid
}

fn read_signal(fd: usize) -> (isize, u32) {
    let mut info = [0u8; 128];
    let count = sys_read(fd, &mut info);
    let signo = u32::from_ne_bytes([info[0], info[1], info[2], info[3]]);
    (count, signo)
}

fn spawn_watchdog(target: usize) -> isize {
    let watchdog = sys_fork();
    if watchdog == 0 {
        sleep(WATCHDOG_DELAY_MS);
        let _ = sys_kill(target, SIGTERM);
        sys_exit(1);
    }
    watchdog
}

fn stop_watchdog(watchdog: isize) {
    if watchdog > 0 {
        let _ = sys_kill(watchdog as usize, SIGKILL);
        let _ = sys_waitpid(watchdog, core::ptr::null_mut());
    }
}

/// 验证普通阻塞 read：生产者在信号入队后必须通知当前 sighand。
fn blocking_read(fd: usize) -> bool {
    let parent = sys_getpid();
    let sender = sys_fork();
    if sender == 0 {
        sleep(SEND_DELAY_MS);
        sys_exit(if sys_kill(parent as usize, SIGUSR1) == 0 {
            0
        } else {
            1
        });
    }
    if sender < 0 {
        return false;
    }

    let watchdog = spawn_watchdog(parent as usize);
    if watchdog < 0 {
        let _ = sys_kill(sender as usize, SIGKILL);
        let _ = reap(sender);
        return false;
    }

    let start = sys_get_time();
    let (count, signo) = read_signal(fd);
    let elapsed = sys_get_time() - start;
    stop_watchdog(watchdog);
    let sender_ok = reap(sender);
    let read_ok = count == 128 && signo == SIGUSR1 as u32;
    println!(
        "    blocking detail: count={} signo={} elapsed={} sender_reaped={}",
        count, signo, elapsed, sender_ok
    );
    read_ok && sender_ok && elapsed >= MIN_BLOCK_MS
}

/// 验证 fork 后继承的 File 不会继续绑定父进程队列。
fn inherited_fd_read(fd: usize) -> bool {
    let mut ready_pipe = [-1i32; 2];
    if sys_pipe(&mut ready_pipe) < 0 {
        return false;
    }
    let mut result_pipe = [-1i32; 2];
    if sys_pipe(&mut result_pipe) < 0 {
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_close(ready_pipe[1] as usize);
        return false;
    }

    let child = sys_fork();
    if child == 0 {
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_close(result_pipe[0] as usize);
        let ready = sys_write(ready_pipe[1] as usize, b"r") == 1;
        let _ = sys_close(ready_pipe[1] as usize);
        let (count, signo) = read_signal(fd);
        let received = ready && count == 128 && signo == SIGUSR1 as u32;
        println!("    child detail: count={} signo={}", count, signo);
        if received {
            let _ = sys_write(result_pipe[1] as usize, b"p");
        }
        let _ = sys_close(result_pipe[1] as usize);
        sys_exit(if received { 0 } else { 1 });
    }
    if child < 0 {
        let _ = sys_close(ready_pipe[0] as usize);
        let _ = sys_close(ready_pipe[1] as usize);
        let _ = sys_close(result_pipe[0] as usize);
        let _ = sys_close(result_pipe[1] as usize);
        return false;
    }

    let _ = sys_close(ready_pipe[1] as usize);
    let _ = sys_close(result_pipe[1] as usize);
    let mut ready = [0u8; 1];
    let child_ready = sys_read(ready_pipe[0] as usize, &mut ready) == 1;
    let _ = sys_close(ready_pipe[0] as usize);
    if !child_ready {
        let _ = sys_close(result_pipe[0] as usize);
        let _ = sys_kill(child as usize, SIGKILL);
        let _ = reap(child);
        return false;
    }

    let watchdog = spawn_watchdog(sys_getpid() as usize);
    if watchdog < 0 {
        let _ = sys_close(result_pipe[0] as usize);
        let _ = sys_kill(child as usize, SIGKILL);
        let _ = reap(child);
        return false;
    }

    // ready byte 只证明 child 已到 read 前；短暂延迟让测试真正覆盖阻塞唤醒。
    sleep(SEND_DELAY_MS);
    let sent = sys_kill(child as usize, SIGUSR1) == 0;
    let mut result = [0u8; 1];
    let result_count = sys_read(result_pipe[0] as usize, &mut result);
    let received = result_count == 1 && result[0] == b'p';
    let _ = sys_close(result_pipe[0] as usize);
    let child_ok = reap(child);
    stop_watchdog(watchdog);
    println!(
        "    inherited detail: ready={} sent={} result_count={} result={} child_reaped={}",
        child_ready, sent, result_count, result[0], child_ok
    );
    sent && received && child_ok
}

pub fn run() -> i32 {
    println!("[regression_signalfd] start");

    let mask = signal_mask(SIGUSR1);
    let mask_ret = sys_rt_sigprocmask(
        SIG_BLOCK,
        &mask,
        core::ptr::null_mut(),
        core::mem::size_of::<u64>(),
    );
    if mask_ret < 0 {
        println!("FAIL: could not block SIGUSR1, ret={}", mask_ret);
        return 1;
    }

    let fd = sys_signalfd4(-1, &mask, core::mem::size_of::<u64>(), 0);
    println!("  setup: mask_ret={} fd={}", mask_ret, fd);
    let (blocking_ok, inherited_ok) = if fd >= 0 {
        (blocking_read(fd as usize), inherited_fd_read(fd as usize))
    } else {
        (false, false)
    };
    println!("  blocking read: {}", if blocking_ok { "PASS" } else { "FAIL" });
    println!("  inherited fd read: {}", if inherited_ok { "PASS" } else { "FAIL" });
    let passed = fd >= 0 && blocking_ok && inherited_ok;

    if fd >= 0 {
        let _ = sys_close(fd as usize);
    }
    let _ = sys_rt_sigprocmask(
        SIG_UNBLOCK,
        &mask,
        core::ptr::null_mut(),
        core::mem::size_of::<u64>(),
    );

    if passed {
        println!("[regression_signalfd] PASS");
        0
    } else {
        println!("FAIL: blocking or inherited signalfd read failed");
        1
    }
}
