extern crate alloc;
use alloc::format;
use user_lib::{exec, exit, fork, println, waitpid};
pub fn run_ltp_suite_runner(environ: &[*const u8], root: &str, suffix: &str, timeout: u64, _conf: Option<&[u8]>) {
    println!("#### OS COMP TEST GROUP START ltp-{} ####", suffix); let pid = fork();
    // Always use initramfs /os_test.conf — the test_runner already parsed the
    // sdcard config for the mask, and the LTP suite lists (ltp_suites/ltp_exclude)
    // are identical in both copies. Passing /sdcard/os_test.conf hits ENOENT in
    // the ltprunner child (likely mount-visibility or fd-exhaustion issue).
    if pid == 0 { let root = format!("{}\0", root); let timeout = format!("{}\0", timeout.saturating_sub(50)); exec("/ltprunner\0", &["/ltprunner\0".as_ptr(), "--conf\0".as_ptr(), "/os_test.conf\0".as_ptr(), "--libc\0".as_ptr(), format!("{}\0", suffix).as_ptr(), "--ltproot\0".as_ptr(), root.as_ptr(), "--group-timeout-secs\0".as_ptr(), timeout.as_ptr(), core::ptr::null()], environ); exit(127); }
    let mut status = 0; if pid > 0 { let _ = waitpid(pid as usize, &mut status); } println!("#### OS COMP TEST GROUP END ltp-{} ####", suffix);
}
