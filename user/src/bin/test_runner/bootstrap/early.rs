use user_lib::{
    exec, exit, fork, get_time, kill, println, sleep, waitpid_wnohang, SIGKILL,
};
use user_lib::syscall::{sys_clock_settime, TimeSpec};

fn sync_time() {
    let pid = fork();
    if pid == 0 {
        exec(
            "/rescue/sh\0",
            &[
                "ntpd\0".as_ptr(),
                "-n\0".as_ptr(),
                "-q\0".as_ptr(),
                "-p\0".as_ptr(),
                "time.cloudflare.com\0".as_ptr(),
                core::ptr::null(),
            ],
            &[core::ptr::null()],
        );
        exit(127);
    }

    let mut status = 0;
    let start = get_time() as usize;
    while pid > 0 && waitpid_wnohang(pid, &mut status) == 0 {
        if (get_time() as usize).saturating_sub(start) >= 3_000 {
            let _ = kill(pid as usize, SIGKILL);
            break;
        }
        sleep(50);
    }

    if status != 0 {
        let _ = sys_clock_settime(
            0,
            &TimeSpec {
                tv_sec: 1749049200,
                tv_nsec: 0,
            },
        );
        println!("[test-runner] ntpd unavailable; using fallback clock");
    }
}

pub fn bootstrap_compat() {
    println!("[test-runner] synchronizing clock");
    sync_time();
}
