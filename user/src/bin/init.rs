#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use user_lib::*;
use user_lib::syscall::sys_mkdirat;

const MS_BIND: usize = 4096;
const AT_FDCWD: isize = -100;

fn try_mount(source: &str, target: &str, fstype: &str, flags: usize, data: usize) -> isize {
    let src_c = format!("{}\0", source);
    let tgt_c = format!("{}\0", target);
    let fs_c = format!("{}\0", fstype);
    mount(src_c.as_ptr(), tgt_c.as_ptr(), fs_c.as_ptr(), flags, data)
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

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[init] MangoCore stage-1 boot (initramfs mode)");

    println!("[init] /dev /proc /tmp mounted by kernel, setting up bind mounts...");

    // 内核已将 x0→/sdcard, x1→/tools 挂载好，直接 bind
    try_bind("/tools/bin", "/bin");
    try_bind("/tools/sbin", "/sbin");
    try_bind("/tools/lib", "/lib");
    try_bind("/tools/usr", "/usr");
    // 不 bind /tools/etc — initramfs 已有完整 /etc，bind 会覆盖
    try_bind("/sdcard/musl", "/musl");
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

    // 尝试进入测试模式
    if try_exec("/sdcard/initproc", environ) || try_exec("/initproc", environ) {
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
