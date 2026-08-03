use user_lib::syscall::{sys_clock_settime, TimeSpec};
use user_lib::{exec, exit, fork, get_time, kill, println, waitpid_wnohang, yield_, SIGKILL};

const NTP_TIMEOUT_MS: usize = 3_000;
const NTP_MAX_WAIT_ROUNDS: usize = 3_000;
const NTP_MAX_REAP_ROUNDS: usize = 64;

fn use_fallback_clock() {
    let _ = sys_clock_settime(
        0,
        &TimeSpec {
            tv_sec: 1749049200,
            tv_nsec: 0,
        },
    );
    println!("[test-runner] ntpd unavailable; using fallback clock");
}

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
    let mut child_exited = false;
    let mut child_waitable = pid > 0;
    let mut ntpd_failed = pid <= 0;
    if pid > 0 {
        let start = get_time() as usize;
        for _ in 0..NTP_MAX_WAIT_ROUNDS {
            let waited = waitpid_wnohang(pid, &mut status);
            if waited == pid {
                child_exited = true;
                ntpd_failed = status != 0;
                break;
            }
            if waited < 0 {
                child_waitable = false;
                ntpd_failed = true;
                break;
            }
            if (get_time() as usize).saturating_sub(start) >= NTP_TIMEOUT_MS {
                ntpd_failed = true;
                break;
            }
            yield_();
        }

        if !child_exited && child_waitable {
            let _ = kill(pid as usize, SIGKILL);
            for _ in 0..NTP_MAX_REAP_ROUNDS {
                if waitpid_wnohang(pid, &mut status) != 0 {
                    break;
                }
                yield_();
            }
            ntpd_failed = true;
        }
    }

    if ntpd_failed {
        use_fallback_clock();
    }
}

pub fn bootstrap_compat() {
    println!("[test-runner] synchronizing clock");
    sync_time();
}
