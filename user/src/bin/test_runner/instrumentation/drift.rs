use crate::runner::config::RuntimeConfig;
use crate::runner::groups::{catalog::TEST_GROUPS, execute::run_group_in_dir};
use crate::runner::process::run_bash_cmd;
use user_lib::{println, sleep};
pub fn run_drift_windows(environ: &[*const u8], cfg: &RuntimeConfig) {
    let libcs: &[&str] = match cfg.drift_libc.as_str() { "musl" => &["musl"], "glibc" => &["glibc"], _ => &["musl", "glibc"] };
    for libc in libcs { println!("[initproc] drift_window: start libc={} windows={}", libc, cfg.drift_windows); for window in 0..cfg.drift_windows { if cfg.drift_pre_mask != 0 { for (index, (name, script)) in TEST_GROUPS.iter().enumerate() { if cfg.drift_pre_mask & (1 << index) != 0 { let dir = alloc::format!("/{}\0", libc); run_group_in_dir(environ, &dir, name, script, cfg.timeouts[index], cfg.diag); } } } println!("[initproc] [drift] === drift_window W{} {} pre ===", window, libc); let command = if cfg.drift_measure == "full" { alloc::format!("cd /{} && sh lmbench_testcode.sh\0", libc) } else { alloc::format!("cd /{} && ./lmbench_all lat_syscall -P 1 null\0", libc) }; let _ = run_bash_cmd(&command, environ); println!("[initproc] [drift] === drift_window W{} {} post ===", window, libc); sleep(100); } }
}
