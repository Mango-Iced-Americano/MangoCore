#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use user_lib::*;
use user_lib::syscall::sys_mkdirat;
use user_lib::syscall::sys_clock_settime;
use user_lib::syscall::TimeSpec;

const MS_BIND: usize = 4096;
const AT_FDCWD: isize = -100;
const NTP_ATTEMPTS: usize = 2;
const NTP_TIMEOUT_MS: usize = 3_000;
const NTP_POLL_MS: usize = 50;

fn try_mount(source: &str, target: &str, fstype: &str, flags: usize, data: usize) -> isize {
    let src_c = format!("{}\0", source);
    let tgt_c = format!("{}\0", target);
    let fs_c = format!("{}\0", fstype);
    mount(src_c.as_ptr(), tgt_c.as_ptr(), fs_c.as_ptr(), flags, data)
}

fn wait_child_bounded(pid: isize, timeout_ms: usize) -> Option<i32> {
    let mut status: i32 = 0;
    let mut waited = 0usize;
    while waited < timeout_ms {
        let ret = waitpid_wnohang(pid, &mut status);
        if ret == pid {
            return Some(status);
        }
        if ret < 0 {
            println!("[init] waitpid_wnohang pid={} failed ret={}", pid, ret);
            return None;
        }
        sleep(NTP_POLL_MS);
        waited += NTP_POLL_MS;
    }

    println!("[init] ntpd pid={} timed out after {}ms, killing", pid, timeout_ms);
    let _ = kill(pid as usize, SIGKILL);
    for _ in 0..20 {
        let ret = waitpid_wnohang(pid, &mut status);
        if ret == pid {
            return Some(status);
        }
        if ret < 0 {
            return None;
        }
        sleep(10);
    }
    None
}

/// 用 busybox ntpd 通过 NTP 同步时间；失败则回退到硬编码时间以防 TLS 失败。
/// NTP is best-effort: early boot must continue even when guest networking or DNS stalls.
fn try_ntp_sync() {
    for attempt in 0..NTP_ATTEMPTS {
        if attempt > 0 {
            sleep(200);
        }

        let pid = fork();
        if pid < 0 {
            // fork 失败，回退到硬编码时间
            set_system_time(1749049200, 0);
            return;
        }
        if pid == 0 {
            // child: run busybox ntpd
            let path = "/rescue/sh\0";
            let applet = "ntpd\0";
            let bg = "-n\0";
            let quit = "-q\0";
            let flag_p = "-p\0";
            let peer = "time.cloudflare.com\0";
            let args: [*const u8; 6] = [
                applet.as_ptr(),
                bg.as_ptr(),
                quit.as_ptr(),
                flag_p.as_ptr(),
                peer.as_ptr(),
                core::ptr::null(),
            ];
            exec(path, &args, &[core::ptr::null()]);
            // exec only returns on error
            exit(-1);
        } else {
            match wait_child_bounded(pid, NTP_TIMEOUT_MS) {
                Some(0) => {
                    println!("[init] ntpd time sync ok");
                    return;
                }
                Some(status) => {
                    println!("[init] ntpd attempt {} failed (status={})", attempt, status);
                }
                None => {
                    println!("[init] ntpd attempt {} did not exit cleanly", attempt);
                }
            }
        }
    }
    println!("[init] ntpd all attempts failed, fallback to hardcoded time");
    set_system_time(1749049200, 0);
}

fn try_bind(source: &str, target: &str) {
    let ret = try_mount(source, target, "", MS_BIND, 0);
    if ret == 0 {
        println!("[init] bind {} -> {}", source, target);
    } else {
        println!("[init] bind {} -> {}: skipped (errno={})", source, target, -ret);
    }
}

fn try_exec(path: &str, environ: &[*const u8]) -> bool {
    let path_c = format!("{}\0", path);
    let args = [path_c.as_ptr(), core::ptr::null()];
    let ret = exec(&path_c, &args, environ);
    if ret < 0 {
        println!("[init] exec {} failed (errno={})", path, -ret);
        false
    } else {
        true
    }
}

fn set_system_time(secs: usize, nsecs: usize) {
    let ts = TimeSpec { tv_sec: secs, tv_nsec: nsecs };
    let ret = sys_clock_settime(0, &ts); // CLOCK_REALTIME=0
    if ret < 0 {
        println!("[init] clock_settime failed: {}", -ret);
    }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[init] MangoCore stage-1 boot (initramfs mode)");

    try_ntp_sync();

    println!("[init] /dev /proc /tmp mounted by kernel, setting up bind mounts...");

    // 内核已将 x0→/sdcard, x1→/tools 挂载好，直接 bind
    // mkdir 保底：确保 target 目录存在（initramfs cpio 可能不含这些目录）
    let _ = sys_mkdirat(AT_FDCWD, "/bin\0", 0o755);
    try_bind("/tools/bin", "/bin");
    let _ = sys_mkdirat(AT_FDCWD, "/sbin\0", 0o755);
    try_bind("/tools/sbin", "/sbin");
    let _ = sys_mkdirat(AT_FDCWD, "/lib\0", 0o755);
    try_bind("/tools/lib", "/lib");
    let _ = sys_mkdirat(AT_FDCWD, "/usr\0", 0o755);
    try_bind("/tools/usr", "/usr");
    let _ = sys_mkdirat(AT_FDCWD, "/tests\0", 0o755);
    try_bind("/tools/tests", "/tests");
    // 不 bind /tools/etc — initramfs 已有完整 /etc，bind 会覆盖
    let _ = sys_mkdirat(AT_FDCWD, "/musl\0", 0o755);
    try_bind("/sdcard/musl", "/musl");
    let _ = sys_mkdirat(AT_FDCWD, "/glibc\0", 0o755);
    try_bind("/sdcard/glibc", "/glibc");

    // /lib 已 bind 到 /tools/lib (ext4)，创建 apk db 目录使其持久化
    for dir in ["/lib/apk\0", "/lib/apk/db\0", "/var/cache/apk\0"] {
        let _ = sys_mkdirat(AT_FDCWD, dir, 0o755);
    }

    let environ: &[*const u8] = &[
        "SHELL=/bin/sh\0".as_ptr(),
        "PWD=/\0".as_ptr(),
        "HOME=/root\0".as_ptr(),
        "PATH=/:/bin:/sbin:/usr/bin:/tools/bin\0".as_ptr(),
        "USER=root\0".as_ptr(),
        core::ptr::null(),
    ];

    // Initramfs carries the freshly built runner; sdcard may still contain an
    // older initproc from the downloaded test image.
    if try_exec("/initproc", environ) || try_exec("/sdcard/initproc", environ) {
        println!("[init] test runner started");
    } else {
        println!("[init] no test runner, entering rescue mode");
        // exec 会替换当前进程，失败才继续下一个
        if !try_exec("/tools/bin/sh", environ)
            && !try_exec("/rescue/sh", environ)
            && !try_exec("/bin/sh", environ)
        {
            println!("[init] FATAL: no shell available");
            println!("[init] System halted.");
            loop {}
        }
    }

    loop {}
}
