#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use user_lib::*;
use user_lib::syscall::{sys_fsync, sys_ftruncate, sys_mkdirat, sys_unlinkat};
use user_lib::syscall::sys_clock_settime;
use user_lib::syscall::TimeSpec;

const MS_BIND: usize = 4096;
const AT_FDCWD: isize = -100;
const NTP_ATTEMPTS: usize = 2;
const NTP_TIMEOUT_MS: usize = 3_000;
const NTP_POLL_MS: usize = 50;
const MIN_VALID_BUILD_EPOCH: usize = 1_704_067_200; // 2024-01-01 UTC
const ENOENT: isize = -2;
const AT_REMOVEDIR: u32 = 0x200;

fn run_scratch_rw_smoke() -> Result<bool, ()> {
    const DIR: &str = "/scratch/MANGO_USR_PROBE\0";
    const FILE: &str = "/scratch/MANGO_USR_PROBE/PAYLOAD.BIN\0";
    const PAYLOAD_LEN: usize = 6144;
    const TRUNCATED_LEN: usize = 2048;

    let mkdir_ret = sys_mkdirat(AT_FDCWD, DIR, 0o755);
    if mkdir_ret == ENOENT {
        println!("[scratch-smoke] /scratch absent, skipping");
        return Ok(false);
    }
    if mkdir_ret != 0 {
        println!("[scratch-smoke] mkdir failed: {}", mkdir_ret);
        return Err(());
    }

    let mut expected = alloc::vec![0u8; PAYLOAD_LEN];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = 0x73 ^ (index as u8).wrapping_mul(0x29) ^ ((index >> 8) as u8);
    }

    let fd = open(FILE, OpenFlags::CREATE | OpenFlags::RDWR | OpenFlags::TRUNC);
    if fd < 0 {
        println!("[scratch-smoke] open for write failed: {}", fd);
        return Err(());
    }
    let written = write(fd as usize, &expected);
    if written != PAYLOAD_LEN as isize {
        println!("[scratch-smoke] write failed: {}", written);
        close(fd as usize);
        return Err(());
    }
    if sys_fsync(fd as usize) != 0 {
        println!("[scratch-smoke] fsync failed");
        close(fd as usize);
        return Err(());
    }
    if sys_ftruncate(fd as usize, TRUNCATED_LEN as isize) != 0 {
        println!("[scratch-smoke] ftruncate failed");
        close(fd as usize);
        return Err(());
    }
    if sys_fsync(fd as usize) != 0 || close(fd as usize) != 0 {
        println!("[scratch-smoke] final sync/close failed");
        return Err(());
    }

    let fd = open(FILE, OpenFlags::RDONLY);
    if fd < 0 {
        println!("[scratch-smoke] reopen failed: {}", fd);
        return Err(());
    }
    let mut actual = alloc::vec![0u8; TRUNCATED_LEN];
    let read_len = read(fd as usize, &mut actual);
    let mut eof = [0u8; 1];
    let eof_len = read(fd as usize, &mut eof);
    close(fd as usize);
    if read_len != TRUNCATED_LEN as isize
        || actual.as_slice() != &expected[..TRUNCATED_LEN]
        || eof_len != 0
    {
        println!(
            "[scratch-smoke] persisted data mismatch: read={} eof={}",
            read_len, eof_len
        );
        return Err(());
    }

    let unlink_ret = sys_unlinkat(AT_FDCWD, FILE, 0);
    if unlink_ret != 0 {
        println!("[scratch-smoke] unlink failed: {}", unlink_ret);
        return Err(());
    }
    let rmdir_ret = sys_unlinkat(AT_FDCWD, DIR, AT_REMOVEDIR);
    if rmdir_ret != 0 {
        println!("[scratch-smoke] rmdir failed: {}", rmdir_ret);
        return Err(());
    }
    println!("[scratch-smoke] PASS: write/fsync/truncate/reopen/read/unlink/rmdir");
    Ok(true)
}

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

fn read_build_epoch() -> Option<usize> {
    let fd = open("/etc/build-epoch\0", OpenFlags::RDONLY);
    if fd < 0 {
        return None;
    }

    let mut buf = [0u8; 32];
    let read_len = read(fd as usize, &mut buf);
    close(fd as usize);
    if read_len <= 0 {
        return None;
    }

    let mut value = 0usize;
    let mut saw_digit = false;
    for &byte in &buf[..read_len as usize] {
        if byte.is_ascii_digit() {
            saw_digit = true;
            value = value.checked_mul(10)?.checked_add((byte - b'0') as usize)?;
        } else if saw_digit && byte.is_ascii_whitespace() {
            break;
        } else {
            return None;
        }
    }
    (saw_digit && value >= MIN_VALID_BUILD_EPOCH).then_some(value)
}

fn set_build_time_fallback(reason: &str) {
    match read_build_epoch() {
        Some(epoch) => {
            println!("[init] {}: fallback to image build epoch {}", reason, epoch);
            set_system_time(epoch, 0);
        }
        None => {
            println!("[init] {}: no valid /etc/build-epoch; keeping kernel clock", reason);
        }
    }
}

/// Use BusyBox ntpd for the authoritative time and the image build timestamp
/// as a lower-bound fallback when early networking or DNS is unavailable.
/// NTP is best-effort: early boot must continue even when guest networking or DNS stalls.
fn try_ntp_sync() {
    for attempt in 0..NTP_ATTEMPTS {
        if attempt > 0 {
            sleep(200);
        }

        let pid = fork();
        if pid < 0 {
            set_build_time_fallback("ntpd fork failed");
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
    set_build_time_fallback("ntpd all attempts failed");
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

    let scratch_rw = match run_scratch_rw_smoke() {
        Ok(available) => available,
        Err(()) => {
            println!("[init] FATAL: writable scratch smoke test failed");
            loop {}
        }
    };

    try_ntp_sync();

    println!("[init] /dev /proc /tmp mounted by kernel, setting up bind mounts...");

    // Keep initramfs runtime directories writable when the staged P2 scratch
    // mount is active. Tools and test payloads remain available at their
    // read-only source paths.
    let _ = sys_mkdirat(AT_FDCWD, "/bin\0", 0o755);
    let _ = sys_mkdirat(AT_FDCWD, "/sbin\0", 0o755);
    let _ = sys_mkdirat(AT_FDCWD, "/lib\0", 0o755);
    let _ = sys_mkdirat(AT_FDCWD, "/usr\0", 0o755);
    if scratch_rw {
        println!("[init] staged runtime: keeping /bin /sbin /lib /usr writable");
    } else {
        try_bind("/tools/bin", "/bin");
        try_bind("/tools/sbin", "/sbin");
        try_bind("/tools/lib", "/lib");
        try_bind("/tools/usr", "/usr");
    }
    let _ = sys_mkdirat(AT_FDCWD, "/tests\0", 0o755);
    try_bind("/tools/tests", "/tests");
    // 不 bind /tools/etc — initramfs 已有完整 /etc，bind 会覆盖
    let _ = sys_mkdirat(AT_FDCWD, "/musl\0", 0o755);
    try_bind("/sdcard/musl", "/musl");
    let _ = sys_mkdirat(AT_FDCWD, "/glibc\0", 0o755);
    try_bind("/sdcard/glibc", "/glibc");

    // These directories live in initramfs during staged board runs and in the
    // legacy tools mount on existing QEMU paths.
    for dir in ["/lib/apk\0", "/lib/apk/db\0", "/var/cache/apk\0"] {
        let _ = sys_mkdirat(AT_FDCWD, dir, 0o755);
    }

    let environ: &[*const u8] = &[
        "SHELL=/bin/sh\0".as_ptr(),
        "PWD=/\0".as_ptr(),
        "HOME=/root\0".as_ptr(),
        "PATH=/:/bin:/sbin:/usr/bin:/usr/sbin:/tools/bin:/tools/sbin:/tools/usr/bin:/tools/usr/sbin\0".as_ptr(),
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
