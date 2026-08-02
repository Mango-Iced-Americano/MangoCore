#![no_std]
#![no_main]

extern crate alloc;
use core::sync::atomic::{AtomicBool, Ordering};
use user_lib::{
    exec, exit, fork, getpid, kill, open, println, read, shutdown, sigaction, sleep,
    waitpid_wnohang, OpenFlags, SigAction, SIGCHLD, SIGINT, SIGKILL, SIGTERM,
};

#[path = "init/mounts.rs"]
mod mounts;
#[path = "init/vf2.rs"]
mod vf2;

const PID1: isize = 1;
const RUNNER: &str = "/test-runner\0";
const RESCUE_SHELL: &str = "/rescue/sh\0";
const SIGACTION_RESTART: usize = 0x10000000;
static CHILD_EVENT: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigchld(_signal: i32) {
    CHILD_EVENT.store(true, Ordering::Release);
}

extern "C" fn on_shutdown(_signal: i32) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
}

fn handler_address(handler: extern "C" fn(i32)) -> usize {
    handler as *const () as usize
}

fn install_signal_handlers() {
    let child = SigAction {
        handler: handler_address(on_sigchld),
        flags: SIGACTION_RESTART,
        restorer: 0,
        mask: 0,
    };
    let shutdown_action = SigAction {
        handler: handler_address(on_shutdown),
        flags: SIGACTION_RESTART,
        restorer: 0,
        mask: 0,
    };
    let _ = sigaction(SIGCHLD, &child);
    let _ = sigaction(SIGINT, &shutdown_action);
    let _ = sigaction(SIGTERM, &shutdown_action);
}

fn reap_orphans() {
    let mut status = 0;
    while waitpid_wnohang(-1, &mut status) > 0 {}
    CHILD_EVENT.store(false, Ordering::Release);
}

fn boot_profile() -> &'static str {
    let fd = open("/proc/cmdline\0", OpenFlags::RDONLY);
    if fd < 0 {
        return "normal";
    }
    let mut cmdline = [0u8; 256];
    let size = read(fd as usize, &mut cmdline);
    let _ = user_lib::close(fd as usize);
    if size > 0
        && cmdline[..size as usize]
            .windows(b"mango.mode=regression".len())
            .any(|v| v == b"mango.mode=regression")
    {
        "regression"
    } else if size > 0
        && cmdline[..size as usize]
            .windows(b"profile=rescue".len())
            .any(|v| v == b"profile=rescue")
    {
        "rescue"
    } else {
        "normal"
    }
}

fn runner_environment(profile: &str) -> [*const u8; 8] {
    let profile_var = match profile {
        "regression" => "MANGO_BOOT_PROFILE=regression\0",
        "rescue" => "MANGO_BOOT_PROFILE=rescue\0",
        _ => "MANGO_BOOT_PROFILE=normal\0",
    };
    [
        "SHELL=/bin/sh\0".as_ptr(),
        "HOME=/root\0".as_ptr(),
        "PATH=/:/bin:/sbin:/usr/bin:/usr/sbin\0".as_ptr(),
        "USER=root\0".as_ptr(),
        "PWD=/\0".as_ptr(),
        profile_var.as_ptr(),
        core::ptr::null(),
        core::ptr::null(),
    ]
}

fn rescue_forever() -> ! {
    println!("[init] entering rescue shell");
    loop {
        let shell = fork();
        if shell == 0 {
            exec(
                RESCUE_SHELL,
                &[RESCUE_SHELL.as_ptr(), core::ptr::null()],
                &[core::ptr::null()],
            );
            exit(127);
        }
        if shell < 0 {
            reap_orphans();
            sleep(100);
            continue;
        }
        let mut status = 0;
        while waitpid_wnohang(shell, &mut status) == 0 {
            reap_orphans();
            sleep(100);
        }
    }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    if getpid() != PID1 {
        return 1;
    }
    install_signal_handlers();
    mounts::prepare_pseudo_fs_framework();
    mounts::mount_pseudo_filesystems();
    let profile = boot_profile();
    vf2::try_boot();
    if profile != "regression" {
        // mount_boot_block_devices() already owns the x0 → /sdcard and x1 →
        // /tools mount policy. PID1 must not mount them again: a second mount
        // returns EBUSY and prevents the bootstrap from seeing the kernel mount.
        mounts::setup_persistent_mounts();
    } else {
        // No block device in regression mode
        mounts::mount_tmpfs("/tmp\0");
    }
    let environ = runner_environment(profile);
    println!(
        "[init] PID1 profile={} runner={}",
        profile,
        RUNNER.trim_end_matches('\0')
    );

    loop {
        let pid = fork();
        if pid == 0 {
            exec(RUNNER, &[RUNNER.as_ptr(), core::ptr::null()], &environ);
            exit(127);
        }
        if pid < 0 {
            println!("[init] MANGO_RUNNER_FAILURE: fork failed ret={}", pid);
            reap_orphans();
            shutdown();
            rescue_forever();
        }

        let mut status = 0;
        loop {
            let waited = waitpid_wnohang(pid, &mut status);
            if waited == pid {
                println!(
                    "[init] MANGO_RUNNER_FAILURE: runner exited status={}",
                    status
                );
                reap_orphans();
                shutdown();
                rescue_forever();
            }
            if waited < 0 {
                println!(
                    "[init] MANGO_RUNNER_FAILURE: runner wait failed ret={}",
                    waited
                );
                reap_orphans();
                shutdown();
                rescue_forever();
            }
            if CHILD_EVENT.load(Ordering::Acquire) {
                reap_orphans();
            }
            if SHUTDOWN_REQUESTED.load(Ordering::Acquire) {
                let _ = kill(pid as usize, SIGKILL);
                reap_orphans();
                shutdown();
                rescue_forever();
            }
            sleep(10);
        }
    }
}
