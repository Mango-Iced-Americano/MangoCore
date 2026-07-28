//! Regression: a wake-one pipe chain must make progress without a timer fallback.
//! The process-per-hop topology matches LTP pipe13 while remaining bounded.

use user_lib::println;
use user_lib::syscall::*;

const HOP_COUNT: usize = 64;
const PIPE_COUNT: usize = HOP_COUNT + 1;
const EPOLL_CTL_ADD: usize = 1;
const EPOLLIN: u32 = 0x001;
const SIGKILL: usize = 9;
const WAIT_TIMEOUT_MS: isize = 5_000;
const WNOHANG: usize = 1;

fn close_except(pipes: &[[i32; 2]; PIPE_COUNT], first: usize, second: usize) {
    for pipe in pipes {
        for fd in pipe {
            let fd = *fd as usize;
            if fd != first && fd != second {
                let _ = sys_close(fd);
            }
        }
    }
}

fn stop_children(pids: &[isize; HOP_COUNT]) {
    for &pid in pids {
        if pid > 0 {
            let _ = sys_kill(pid as usize, SIGKILL);
        }
    }
}

fn reap_children(pids: &[isize; HOP_COUNT], timeout_ms: isize) -> bool {
    let deadline = sys_get_time() + timeout_ms;
    let mut reaped = [false; HOP_COUNT];
    let mut remaining = HOP_COUNT;

    while remaining != 0 && sys_get_time() < deadline {
        for index in 0..HOP_COUNT {
            if reaped[index] {
                continue;
            }
            let mut status = 0;
            let ret = sys_waitpid_flags(pids[index], &mut status, WNOHANG);
            if ret == pids[index] {
                reaped[index] = true;
                remaining -= 1;
            } else if ret < 0 {
                return false;
            }
        }
        if remaining != 0 {
            let _ = sys_yield();
        }
    }

    remaining == 0
}

pub fn run() -> i32 {
    println!("[regression_pipe_wakeup] start: {} hops", HOP_COUNT);

    let mut pipes = [[-1i32; 2]; PIPE_COUNT];
    for pipe in &mut pipes {
        let ret = sys_pipe(pipe);
        if ret < 0 {
            println!("FAIL: pipe() returned {}", ret);
            close_except(&pipes, usize::MAX, usize::MAX - 1);
            return 1;
        }
    }

    let mut children = [0isize; HOP_COUNT];
    for index in 0..HOP_COUNT {
        let child = sys_fork();
        if child < 0 {
            println!("FAIL: fork at hop {} returned {}", index, child);
            stop_children(&children);
            close_except(&pipes, usize::MAX, usize::MAX - 1);
            let _ = reap_children(&children, WAIT_TIMEOUT_MS);
            return 1;
        }
        if child == 0 {
            let read_fd = pipes[index][0] as usize;
            let write_fd = pipes[index + 1][1] as usize;
            close_except(&pipes, read_fd, write_fd);

            let mut byte = [0u8; 1];
            let read_count = sys_read(read_fd, &mut byte);
            let write_count = if read_count == 1 {
                sys_write(write_fd, &byte)
            } else {
                -1
            };
            let _ = sys_close(read_fd);
            let _ = sys_close(write_fd);
            sys_exit(if write_count == 1 { 0 } else { 1 });
        }
        children[index] = child;
    }

    let input_fd = pipes[0][1] as usize;
    let output_fd = pipes[HOP_COUNT][0] as usize;
    close_except(&pipes, input_fd, output_fd);

    let epfd = sys_epoll_create1(0);
    if epfd < 0 {
        println!("FAIL: epoll_create1 returned {}", epfd);
        let _ = sys_close(input_fd);
        let _ = sys_close(output_fd);
        stop_children(&children);
        let _ = reap_children(&children, WAIT_TIMEOUT_MS);
        return 1;
    }
    let interest = EpollEvent {
        events: EPOLLIN,
        data: output_fd as u64,
    };
    if sys_epoll_ctl(epfd as usize, EPOLL_CTL_ADD, output_fd, &interest) < 0 {
        println!("FAIL: epoll_ctl failed");
        let _ = sys_close(epfd as usize);
        let _ = sys_close(input_fd);
        let _ = sys_close(output_fd);
        stop_children(&children);
        let _ = reap_children(&children, WAIT_TIMEOUT_MS);
        return 1;
    }

    if sys_write(input_fd, &[0x5A]) != 1 {
        println!("FAIL: initial pipe write failed");
        let _ = sys_close(input_fd);
        let _ = sys_close(output_fd);
        let _ = sys_close(epfd as usize);
        stop_children(&children);
        let _ = reap_children(&children, WAIT_TIMEOUT_MS);
        return 1;
    }
    let _ = sys_close(input_fd);

    let mut event = EpollEvent { events: 0, data: 0 };
    let ready = sys_epoll_pwait(epfd as usize, &mut event, 1, WAIT_TIMEOUT_MS);
    let mut output = [0u8; 1];
    let read_count = if ready == 1 && event.events & EPOLLIN != 0 {
        sys_read(output_fd, &mut output)
    } else {
        -1
    };
    let _ = sys_close(output_fd);
    let _ = sys_close(epfd as usize);

    let complete = read_count == 1 && output == [0x5A] && reap_children(&children, WAIT_TIMEOUT_MS);
    if !complete {
        println!("FAIL: chain ready={} read={} output={:?}", ready, read_count, output);
        stop_children(&children);
        let _ = reap_children(&children, WAIT_TIMEOUT_MS);
        return 1;
    }

    println!("[regression_pipe_wakeup] PASS");
    0
}
