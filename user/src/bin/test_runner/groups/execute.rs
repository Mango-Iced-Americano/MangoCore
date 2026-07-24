extern crate alloc;
use alloc::format;
use crate::runner::config::{LtpLibc, LtpRunner, RuntimeConfig};
use crate::runner::groups::catalog::TEST_GROUPS;
use crate::runner::ltp::{inline::run_ltp_binaries, suite::run_ltp_suite_runner};
use user_lib::{chdir, exec, exit, fork, get_time, getpgid, kill, println, setpgid, sleep, waitpid, waitpid_wnohang, SIGKILL};
pub fn run_group_in_dir(environ: &[*const u8], dir: &str, group: &str, script: &str, timeout: u64) {
    let suffix = if dir.contains("musl") { "musl" } else if group == "cpython" { "isolated" } else { "glibc" };
    println!("#### OS COMP TEST GROUP START {}-{} ####", group, suffix);
    let pid = fork(); if pid == 0 { let _ = setpgid(0, 0); if chdir(dir) < 0 { exit(126); } let command = format!("./{}\0", script); exec("/bin/bash\0", &["/bin/bash\0".as_ptr(), "-c\0".as_ptr(), command.as_ptr(), core::ptr::null()], environ); exit(127); }
    let start = get_time() as u64; let mut status = 0;
    while pid > 0 && waitpid_wnohang(pid, &mut status) == 0 { if (get_time() as u64).saturating_sub(start) >= timeout * 1000 { let pgid = getpgid(pid as usize); if pgid > 0 { let _ = kill(!(pgid as usize) + 1, SIGKILL); } let _ = kill(pid as usize, SIGKILL); let _ = waitpid(pid as usize, &mut status); break; } sleep(100); }
    println!("#### OS COMP TEST GROUP END {}-{} ####", group, suffix); println!("[initproc] done {} in {} exit_code={}", script, dir.trim_end_matches('\0'), status);
}
pub fn run_selected_groups(environ: &[*const u8], cfg: &RuntimeConfig) {
    println!("[initproc] run_selected_groups start mask=0x{:03X}", cfg.mask);
    for &index in &cfg.order { let (group, script) = TEST_GROUPS[index]; if cfg.mask & (1 << index) == 0 { println!("[initproc] skip {} (mask bit{} not set)", group, index); continue; }
        if group == "ltp" && cfg.ltp_runner == LtpRunner::Inline { if cfg.ltp_libc != LtpLibc::Glibc { run_ltp_binaries(environ, "/musl\0", &cfg.ltp_exclude, &cfg.ltp_include, cfg.ltp_from.as_deref(), cfg.timeouts[index]); } if cfg.ltp_libc != LtpLibc::Musl { run_ltp_binaries(environ, "/glibc\0", &cfg.ltp_exclude, &cfg.ltp_include, cfg.ltp_from.as_deref(), cfg.timeouts[index]); } }
        else if group == "ltp" && cfg.ltp_runner == LtpRunner::Suite { run_ltp_suite_runner(environ, "/musl/ltp", "musl", cfg.timeouts[index], cfg.conf_source.as_deref()); run_ltp_suite_runner(environ, "/glibc/ltp", "glibc", cfg.timeouts[index], cfg.conf_source.as_deref()); }
        else if group == "cpython" { run_group_in_dir(environ, "/tools/tests/cpython\0", group, script, cfg.timeouts[index]); }
        else { run_group_in_dir(environ, "/musl\0", group, script, cfg.timeouts[index]); run_group_in_dir(environ, "/glibc\0", group, script, cfg.timeouts[index]); }
        sleep(1000); }
    println!("[initproc] run_selected_groups done");
}
