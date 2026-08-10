#![no_std]
#![no_main]

extern crate alloc;
use alloc::format;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};
use user_lib::syscall::{sys_chroot, sys_mkdirat, sys_sched_setaffinity};
use user_lib::{
    chdir, close, exec, exit, fork, getpid, kill, mount, open, println, read, shutdown, sigaction,
    sleep, waitpid_wnohang, write, OpenFlags, SigAction, SIGCHLD, SIGINT, SIGKILL, SIGTERM,
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

fn write_buildstorm_stat_control(path: &str, value: &[u8]) -> bool {
    let fd = open(path, OpenFlags::WRONLY);
    if fd < 0 {
        println!("[init] BuildStorm stats open {} failed: {}", path, fd);
        return false;
    }
    let written = write(fd as usize, value);
    let _ = close(fd as usize);
    if written != value.len() as isize {
        println!("[init] BuildStorm stats write {} failed: {}", path, written);
        return false;
    }
    true
}

fn buildstorm_stats_profile() -> Option<(&'static str, &'static [u8], bool)> {
    if boot_cmdline_contains(b"buildstorm.stats=all") {
        Some(("all", b"all\n", true))
    } else if boot_cmdline_contains(b"buildstorm.stats=memory_io") {
        Some(("memory_io", b"memory_io\n", true))
    } else if boot_cmdline_contains(b"buildstorm.stats=core") {
        Some(("core", b"core\n", false))
    } else {
        None
    }
}

fn enable_buildstorm_stats(profile_name: &str, profile_value: &[u8]) {
    let profile_ok = write_buildstorm_stat_control("/sys/kernel/stats/profile\0", profile_value);
    let reset_ok = write_buildstorm_stat_control("/sys/kernel/stats/reset\0", b"1\n");
    let stats_on_ok = write_buildstorm_stat_control("/sys/kernel/stats/stats_on\0", b"1\n");
    println!(
        "[init] BuildStorm stats enabled profile={} profile_ok={} reset_ok={} stats_on_ok={}",
        profile_name, profile_ok, reset_ok, stats_on_ok
    );
}

fn dump_buildstorm_stats(sample: usize, all: bool) {
    println!("BUILDSTORM_STATS_BEGIN sample={}", sample);
    let paths: &[&str] = if all {
        &[
            "/sys/kernel/stats/taskq\0",
            "/sys/kernel/stats/pagecache\0",
            "/sys/kernel/stats/blockio\0",
            "/sys/kernel/stats/vm\0",
            "/sys/kernel/stats/heap\0",
            "/sys/kernel/stats/pagefault\0",
            "/sys/kernel/stats/ext4\0",
            "/sys/kernel/stats/mount\0",
            "/sys/kernel/stats/net\0",
        ]
    } else {
        &["/sys/kernel/stats/taskq\0"]
    };
    for path in paths {
        let fd = open(path, OpenFlags::RDONLY);
        if fd < 0 {
            println!("[init] BuildStorm stats read {} failed: {}", path, fd);
            continue;
        }
        let mut buf = [0u8; 16384];
        let size = read(fd as usize, &mut buf);
        let _ = close(fd as usize);
        if size > 0 {
            let _ = write(1, &buf[..size as usize]);
        }
    }
    println!("BUILDSTORM_STATS_END sample={}", sample);
}

fn start_buildstorm_stats_collector(all: bool) {
    let child = fork();
    if child == 0 {
        let mut sample = 0;
        let (period_ms, dump_all) = if all {
            (30_000, true)
        } else {
            (5_000, false)
        };
        loop {
            dump_buildstorm_stats(sample, dump_all);
            sample = sample.wrapping_add(1);
            sleep(period_ms);
        }
    }
    if child < 0 {
        println!("[init] BuildStorm stats collector fork failed: {}", child);
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

fn read_boot_cmdline(buf: &mut [u8]) -> usize {
    let fd = open("/proc/cmdline\0", OpenFlags::RDONLY);
    if fd < 0 {
        return 0;
    }
    let size = read(fd as usize, buf);
    let _ = close(fd as usize);
    size.max(0) as usize
}

fn cmdline_contains(cmdline: &[u8], needle: &[u8]) -> bool {
    cmdline.windows(needle.len()).any(|value| value == needle)
}

fn boot_cmdline_contains(needle: &[u8]) -> bool {
    let mut cmdline = [0u8; 256];
    let size = read_boot_cmdline(&mut cmdline);
    cmdline_contains(&cmdline[..size], needle)
}

fn boot_profile() -> &'static str {
    let mut cmdline = [0u8; 256];
    let size = read_boot_cmdline(&mut cmdline);
    let cmdline = &cmdline[..size];
    if cmdline_contains(cmdline, b"mango.mode=regression") {
        "regression"
    } else if cmdline_contains(cmdline, b"profile=buildstorm") {
        "buildstorm"
    } else if cmdline_contains(cmdline, b"profile=rescue") {
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
    let buildstorm_stats = if profile == "buildstorm" {
        buildstorm_stats_profile()
    } else {
        None
    };
    if profile == "buildstorm" {
        // mount_boot_block_devices() already owns the x0 → /sdcard mount.
        // PID1 must reuse it so the chroot does not fail on a second EBUSY mount.
        mounts::mount_tmpfs("/tmp\0");
        if !enter_buildstorm_root() {
            println!("[init] BuildStorm root unavailable; entering rescue");
            rescue_forever();
        }
        if let Some((profile_name, profile_value, all)) = buildstorm_stats {
            enable_buildstorm_stats(profile_name, profile_value);
            start_buildstorm_stats_collector(all);
        }
        enable_buildstorm_cpus();
        exec_buildstorm_init();
    }
    if profile != "regression" {
        // mount_boot_block_devices() already owns x0 → /sdcard and x1 → /tools.
        // Bind their persistent subtrees instead of remounting the same devices.
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
