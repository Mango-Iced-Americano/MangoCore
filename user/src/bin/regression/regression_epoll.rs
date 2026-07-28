//! Regression: epoll waits wake only when a watched producer makes the pipe readable.

use user_lib::{println, sleep};
use user_lib::syscall::*;

const EPOLL_CTL_ADD: usize = 1;
const EPOLLIN: u32 = 0x001;
const WRITER_DELAY_MS: usize = 100;
const WATCHDOG_DELAY_MS: usize = 5_000;
const TIMEOUT_MS: isize = 100;
const SIGKILL: usize = 9;
const SIGTERM: usize = 15;

fn reap(pid: isize) -> bool {
    let mut status = 0;
    sys_waitpid(pid, &mut status) == pid && status == 0
}

pub fn run() -> i32 {
    println!("[regression_epoll] start");

    let mut pipe = [-1i32; 2];
    if sys_pipe(&mut pipe) < 0 {
        println!("FAIL: pipe failed");
        return 1;
    }
    let epfd = sys_epoll_create1(0);
    if epfd < 0 {
        println!("FAIL: epoll_create1 returned {}", epfd);
        let _ = sys_close(pipe[0] as usize);
        let _ = sys_close(pipe[1] as usize);
        return 1;
    }
    let interest = EpollEvent { events: EPOLLIN, data: pipe[0] as u64 };
    if sys_epoll_ctl(epfd as usize, EPOLL_CTL_ADD, pipe[0] as usize, &interest) < 0 {
        println!("FAIL: epoll_ctl add failed");
        let _ = sys_close(epfd as usize);
        let _ = sys_close(pipe[0] as usize);
        let _ = sys_close(pipe[1] as usize);
        return 1;
    }

    let writer = sys_fork();
    if writer == 0 {
        let _ = sys_close(pipe[0] as usize);
        sleep(WRITER_DELAY_MS);
        let written = sys_write(pipe[1] as usize, b"x");
        let _ = sys_close(pipe[1] as usize);
        sys_exit(if written == 1 { 0 } else { 1 });
    }
    if writer < 0 {
        println!("FAIL: writer fork returned {}", writer);
        let _ = sys_close(epfd as usize);
        let _ = sys_close(pipe[0] as usize);
        let _ = sys_close(pipe[1] as usize);
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
        let _ = sys_close(epfd as usize);
        let _ = sys_close(pipe[0] as usize);
        let _ = sys_close(pipe[1] as usize);
        return 1;
    }

    let mut event = EpollEvent { events: 0, data: 0 };
    let ready = sys_epoll_pwait(epfd as usize, &mut event, 1, -1);
    let mut byte = [0u8; 1];
    let read_count = if ready == 1 && event.events & EPOLLIN != 0 {
        sys_read(pipe[0] as usize, &mut byte)
    } else {
        -1
    };
    let _ = sys_kill(watchdog as usize, SIGKILL);
    let writer_ok = reap(writer);
    let _ = reap(watchdog);
    let timed_out = sys_epoll_pwait(epfd as usize, &mut event, 1, TIMEOUT_MS);
    let _ = sys_close(epfd as usize);
    let _ = sys_close(pipe[0] as usize);
    let _ = sys_close(pipe[1] as usize);

    if ready != 1 || event.events & EPOLLIN == 0 || read_count != 1 || byte != *b"x" || !writer_ok || timed_out != 0 {
        println!("FAIL: ready={} events=0x{:x} read={} writer_ok={} timeout={}", ready, event.events, read_count, writer_ok, timed_out);
        return 1;
    }

    println!("[regression_epoll] PASS");
    0
}
