extern crate alloc;
use alloc::format;
use user_lib::{exec, exit, fork, get_time, kill, mount, println, sleep, waitpid_wnohang, SIGKILL};
use user_lib::syscall::{sys_clock_settime, sys_mkdirat, TimeSpec};
const MS_BIND: usize = 4096; const AT_FDCWD: isize = -100;
fn bind(source: &str, target: &str) { let source = format!("{}\0", source); let target = format!("{}\0", target); /* idempotent: re-mount returns EBUSY, silently ignored */ let _ = mount(source.as_ptr(), target.as_ptr(), "\0".as_ptr(), MS_BIND, 0); }
fn sync_time() {
    let pid = fork(); if pid == 0 { exec("/rescue/sh\0", &["ntpd\0".as_ptr(), "-n\0".as_ptr(), "-q\0".as_ptr(), "-p\0".as_ptr(), "time.cloudflare.com\0".as_ptr(), core::ptr::null()], &[core::ptr::null()]); exit(127); }
    let mut status = 0; let start = get_time() as usize; while pid > 0 && waitpid_wnohang(pid, &mut status) == 0 { if (get_time() as usize).saturating_sub(start) >= 3_000 { let _ = kill(pid as usize, SIGKILL); break; } sleep(50); }
    if status != 0 { let _ = sys_clock_settime(0, &TimeSpec { tv_sec: 1749049200, tv_nsec: 0 }); println!("[test-runner] ntpd unavailable; using fallback clock"); }
}
pub fn bootstrap_compat() { println!("[test-runner] bootstrap compatibility setup"); sync_time(); for (source, target) in [("/tools/tmp", "/tmp"), ("/tools/bin", "/bin"), ("/tools/sbin", "/sbin"), ("/tools/lib", "/lib"), ("/tools/usr", "/usr"), ("/tools/root", "/root"), ("/tools/tests", "/tests"), ("/sdcard/musl", "/musl"), ("/sdcard/glibc", "/glibc")] { let _ = sys_mkdirat(AT_FDCWD, format!("{}\0", target).as_str(), 0o755); bind(source, target); } }
