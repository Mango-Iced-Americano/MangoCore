#![no_std]
#![no_main]

extern crate alloc;
use alloc::format;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};
use user_lib::syscall::{sys_chroot, sys_mkdirat, sys_mount, sys_sched_setaffinity};
use user_lib::{
    chdir, exec, exit, fork, getpid, kill, mount, open, println, read, shutdown, sigaction, sleep,
    waitpid_wnohang, OpenFlags, SigAction, SIGCHLD, SIGINT, SIGKILL, SIGTERM,
};

#[path = "init/mounts.rs"]
mod mounts;
#[path = "init/vf2.rs"]

const PID1: isize = 1;
const MS_BIND: usize = 4096;
const RUNNER: &str = "/test-runner\0";
const RESCUE_SHELL: &str = "/rescue/sh\0";
const BUILDSTORM_INIT: &str = "/sbin/init\0";
const BUILDSTORM_FALLBACK_INIT: &str = "/init\0";
const BUILDSTORM_SCRIPT: &str = "/glibc/buildstorm_testcode.sh\0";
const BUILDSTORM_SHELL: &str = "/bin/sh\0";
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

fn try_mount(source: &'static str, target: &'static str, fstype: &'static str) -> bool {
    let result = mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), 0, 0);
    if result < 0 {
        println!(
            "[init] mount {} at {} failed: {}",
            fstype.trim_end_matches('\0'),
            target.trim_end_matches('\0'),
            result
        );
        return false;
    }
    true
}

fn mount_disk(source: &'static str, target: &'static str) -> bool {
    try_mount(source, target, "ext4\0") || try_mount(source, target, "fat32\0")
}

fn try_bind_mount(source: &str, target: &str) -> bool {
    let src = alloc::format!("{}\0", source);
    let tgt = alloc::format!("{}\0", target);
    let ret = mount(src.as_ptr(), tgt.as_ptr(), "\0".as_ptr(), MS_BIND, 0);
    if ret == 0 {
        println!("[init] bind mount {} -> {}", source, target);
        true
    } else {
        println!(
            "[init] bind mount {} -> {}: skipped (errno={})",
            source, target, -ret
        );
        false
    }
}

fn root_path(root: &str, suffix: &str) -> alloc::string::String {
    format!("{}{}\0", root.trim_end_matches('\0'), suffix)
}

/// Prepare the BuildStorm chroot with only the host pseudo-filesystems.
/// The official x0 tree must remain intact; `/tools` is deliberately absent.
fn bind_buildstorm_pseudo_filesystems(root: &str) -> bool {
    for suffix in ["/proc", "/sys", "/dev", "/tmp"] {
        let target = root_path(root, suffix);
        let _ = sys_mkdirat(-100, &target, 0o755);
    }
    let mut mounted = true;
    for (source, suffix) in [
        ("/proc", "/proc"),
        ("/sys", "/sys"),
        ("/dev", "/dev"),
        ("/tmp", "/tmp"),
    ] {
        let target = root_path(root, suffix);
        mounted &= try_bind_mount(source, &target);
    }
    mounted
}

fn enter_buildstorm_root() -> bool {
    if !bind_buildstorm_pseudo_filesystems("/sdcard") {
        println!("[init] BuildStorm pseudo-filesystem setup failed");
        return false;
    }
    if chdir("/\0") < 0 {
        println!("[init] BuildStorm pre-chroot chdir failed");
        return false;
    }
    let ret = sys_chroot("/sdcard\0");
    if ret < 0 || chdir("/\0") < 0 {
        println!("[init] BuildStorm chroot failed: {}", ret);
        return false;
    }
    println!("[init] BuildStorm profile entered /sdcard chroot");
    true
}

fn enable_buildstorm_cpus() {
    let mask = usize::MAX;
    let ret = sys_sched_setaffinity(0, size_of::<usize>(), &mask as *const usize as *const u8);
    if ret < 0 {
        println!("[init] BuildStorm sched_setaffinity(all) failed: {}", ret);
    } else {
        println!("[init] BuildStorm sched_setaffinity(all) enabled");
    }
}

fn exec_buildstorm_init() -> ! {
    let env = runner_environment("buildstorm");
    // Official x0 images may ship a generic /sbin/init shell. Prefer the
    // compatibility ladder when present, then retain init/shell fallbacks.
    for path in [
        BUILDSTORM_SCRIPT,
        BUILDSTORM_INIT,
        BUILDSTORM_FALLBACK_INIT,
        BUILDSTORM_SHELL,
    ] {
        println!("[init] BuildStorm exec {}", path.trim_end_matches('\0'));
        let ret = exec(path, &[path.as_ptr(), core::ptr::null()], &env);
        println!(
            "[init] BuildStorm exec {} failed: {}",
            path.trim_end_matches('\0'),
            ret
        );
    }
    loop {
        sleep(1000);
    }
}

fn bind_tools_and_sdcard(tools_ok: bool, disk_ok: bool) {
    // Tools disk: bind-mount key directories to root so writes persist across reboots.
    if tools_ok {
        for (src, dst) in [
            ("/tools/bin", "/bin"),
            ("/tools/sbin", "/sbin"),
            ("/tools/lib", "/lib"),
            ("/tools/usr", "/usr"),
            ("/tools/root", "/root"),
        ] {
            try_bind_mount(src, dst);
        }
    }
    // sdcard: bind-mount musl/glibc runtime directories.
    if disk_ok {
        for (src, dst) in [("/sdcard/musl", "/musl"), ("/sdcard/glibc", "/glibc")] {
            try_bind_mount(src, dst);
        }
    }
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
            .windows(b"profile=buildstorm".len())
            .any(|v| v == b"profile=buildstorm")
    {
        "buildstorm"
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
        "buildstorm" => "MANGO_BOOT_PROFILE=buildstorm\0",
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
    if profile == "buildstorm" {
        let disk_ok = mount_disk("/dev/vda\0", "/sdcard\0");
        // BuildStorm owns the complete x0 userspace.  Give it a fresh tmpfs
        // and expose only the pseudo-filesystems needed by its toolchain.
        let _ = try_mount("none\0", "/tmp\0", "tmpfs\0");
        if !disk_ok || !enter_buildstorm_root() {
            println!("[init] BuildStorm root unavailable; entering rescue");
            rescue_forever();
        }
        enable_buildstorm_cpus();
        exec_buildstorm_init();
    }
    if profile != "regression" {
        let disk_ok = mount_disk("/dev/vda\0", "/sdcard\0");
        let tools_ok =
            mount_disk("/dev/vdb1\0", "/tools\0") || mount_disk("/dev/vdb\0", "/tools\0");
        // /tmp: prefer ext4-backed /tmp if a block device is available
        if disk_ok {
            const AT_FDCWD: isize = -100;
            let _ = sys_mkdirat(AT_FDCWD, "/sdcard/tmp\0", 0o1777);
            let result = sys_mount(
                "/sdcard/tmp\0".as_ptr(),
                "/tmp\0".as_ptr(),
                core::ptr::null(),
                MS_BIND,
                0,
            );
            if result < 0 {
                println!(
                    "[init] bind-mount /sdcard/tmp → /tmp failed: {}, falling back to tmpfs",
                    result
                );
                let _ = try_mount("none\0", "/tmp\0", "tmpfs\0");
            } else {
                println!("[init] /tmp is bind-mounted from ext4 /sdcard/tmp");
            }
        } else {
            let _ = try_mount("none\0", "/tmp\0", "tmpfs\0");
        }
        if tools_ok {
            let result = sys_mount(
                "/tools/etc\0".as_ptr(),
                "/etc\0".as_ptr(),
                core::ptr::null(),
                MS_BIND,
                0,
            );
            if result < 0 {
                println!(
                    "[init] bind-mount /tools/etc → /etc failed: {}, keeping initramfs /etc",
                    result
                );
            } else {
                println!("[init] /etc is bind-mounted from tools disk");
            }
        }
        // Bind-mount tools and sdcard subdirectories so writes persist.
        bind_tools_and_sdcard(tools_ok, disk_ok);
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
