pub mod bootstrap;
pub mod config;
pub mod groups;
pub mod instrumentation;
pub mod ltp;
pub mod process;
pub mod shell;
pub mod smoke;
pub mod vf2;
pub mod vf2_mounts;

pub fn main(argc: usize, argv: &[&str]) -> i32 {
    use core::sync::atomic::Ordering;
    use user_lib::{exit, exec, fork, getpid, getppid, println, shutdown, waitpid};

    if getpid() == 1 || getppid() != 1 {
        println!("[test-runner] lifecycle violation pid={} ppid={}; expected non-PID1 child of init", getpid(), getppid());
        return 1;
    }
    let environ = config::environment();
    bootstrap::early::bootstrap_compat();
    let cfg = config::load_runtime_config();
    if cfg.mode == config::RunMode::Regression {
        println!("[initproc] regression mode: running /regression");
        let pid = fork();
        if pid == 0 {
            exec("/regression\0", &["/regression\0".as_ptr(), core::ptr::null()], &environ);
            exit(127);
        }
        let mut status = 0;
        if pid > 0 { let _ = waitpid(pid as usize, &mut status); }
        shutdown();
        return 0;
    }
    bootstrap::layout::prepare_layout(&environ);
    bootstrap::libraries::link_libraries(&environ);
    bootstrap::libraries::install_embedded_libgcc_s();
    if !bootstrap::packages::install_apk_packages(&environ, cfg.skip_apk) {
        println!("[initproc] aborting test runner: APK package installation failed");
        return 1;
    }
    HAS_BIN_BASH.store(process::run_bash_cmd("test -x /bin/bash\0", &environ) == 0, Ordering::Relaxed);
    if cfg.timer_smoke && !smoke::timerfd::run_timerfd_smoke() { shutdown(); return 1; }
    match cfg.mode {
        // On a real VF2 board try_boot() chroots into the SD root and never
        // returns; on QEMU it returns and the interactive shell starts as before.
        config::RunMode::Shell => {
            vf2::try_boot();
            shell::enter_shell("/bin/bash\0", &environ);
        }
        config::RunMode::DriftWindow => instrumentation::drift::run_drift_windows(&environ, &cfg),
        config::RunMode::Run => {
            groups::execute::run_selected_groups(&environ, &cfg);
        }
        config::RunMode::RunThenShell => {
            groups::execute::run_selected_groups(&environ, &cfg);
            vf2::try_boot();
            shell::enter_shell("/bin/bash\0", &environ);
        }
        config::RunMode::Regression => {}
    }
    let _ = (argc, argv);
    shutdown();
    0
}

use core::sync::atomic::AtomicBool;
pub(crate) static HAS_BIN_BASH: AtomicBool = AtomicBool::new(true);

#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    extern "C" { fn _parameter(argc: usize, argv: usize) -> !; }
    unsafe { _parameter(0, 0) }
}
