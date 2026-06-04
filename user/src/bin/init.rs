#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use user_lib::*;

const MS_BIND: usize = 4096;

fn try_mount(source: &str, target: &str, fstype: &str, flags: usize, data: usize) -> isize {
    let src_c = format!("{}\0", source);
    let tgt_c = format!("{}\0", target);
    let fs_c = format!("{}\0", fstype);
    mount(src_c.as_ptr(), tgt_c.as_ptr(), fs_c.as_ptr(), flags, data)
}

fn try_bind(source: &str, target: &str) -> bool {
    let ret = try_mount(source, target, "", MS_BIND, 0);
    if ret == 0 {
        println!("[init] bind mount {} -> {}", source, target);
        true
    } else {
        println!("[init] bind mount {} -> {}: skipped (errno={})", source, target, -ret);
        false
    }
}

fn file_exists(path: &str) -> bool {
    let fd = open(path, OpenFlags::RDONLY);
    if fd >= 0 {
        close(fd as usize);
        true
    } else {
        false
    }
}

fn try_mount_block(device_path: &str, target: &str, label: &str) -> bool {
    for fs_type in &["ext4", "vfat"] {
        let ret = try_mount(device_path, target, fs_type, 0, 0);
        if ret == 0 {
            println!("[init] mounted {} ({}) at {}", label, fs_type, target);
            return true;
        }
    }
    println!("[init] {} not mounted at {} (no filesystem detected)", label, target);
    false
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[init] MangoCore stage-1 boot (initramfs mode)");

    println!("[init] checking /dev, /proc, /tmp ...");
    if file_exists("/dev") { println!("[init]   /dev: OK"); }
    else { println!("[init]   /dev: MISSING"); }
    if file_exists("/proc") { println!("[init]   /proc: OK"); }
    else { println!("[init]   /proc: MISSING"); }
    if file_exists("/tmp") { println!("[init]   /tmp: OK"); }
    else { println!("[init]   /tmp: MISSING"); }

    if file_exists("/dev/vda") {
        println!("[init] attempting to mount /dev/vda -> /sdcard ...");
        let _ = try_mount_block("/dev/vda", "/sdcard", "official fs (x0)");
    } else {
        println!("[init] /dev/vda not found, skipping /sdcard mount");
    }

    if file_exists("/dev/vdb") {
        println!("[init] attempting to mount /dev/vdb -> /tools ...");
        let _ = try_mount_block("/dev/vdb", "/tools", "tools disk (x1)");
    } else {
        println!("[init] /dev/vdb not found, skipping /tools mount");
    }

    if file_exists("/tools/bin") { try_bind("/tools/bin", "/bin"); }
    if file_exists("/tools/lib") { try_bind("/tools/lib", "/lib"); }
    if file_exists("/tools/usr") { try_bind("/tools/usr", "/usr"); }
    if file_exists("/tools/etc") { try_bind("/tools/etc", "/etc"); }
    if file_exists("/sdcard/musl") { try_bind("/sdcard/musl", "/musl"); }
    if file_exists("/sdcard/glibc") { try_bind("/sdcard/glibc", "/glibc"); }

    let environ: &[*const u8] = &[
        "SHELL=/bin/sh\0".as_ptr(),
        "PWD=/\0".as_ptr(),
        "HOME=/root\0".as_ptr(),
        "PATH=/:/bin:/sbin:/usr/bin:/tools/bin\0".as_ptr(),
        "USER=root\0".as_ptr(),
        core::ptr::null(),
    ];

    let test_elf = if file_exists("/sdcard/initproc") {
        "/sdcard/initproc"
    } else if file_exists("/initproc") {
        "/initproc"
    } else {
        ""
    };

    if !test_elf.is_empty() {
        println!("[init] entering test mode: exec {}", test_elf);
        let elf_c = format!("{}\0", test_elf);
        let args = [elf_c.as_ptr(), core::ptr::null()];
        let _ = exec(&elf_c, &args, environ);
        println!("[init] exec test runner returned, entering rescue");
    } else {
        println!("[init] no test runner found, entering rescue mode");
    }

    let rescue_shells = ["/tools/bin/sh", "/rescue/sh", "/bin/sh"];
    for &shell in &rescue_shells {
        if file_exists(shell) {
            println!("[init] entering rescue shell: {}", shell);
            let shell_c = format!("{}\0", shell);
            let args = [shell_c.as_ptr(), core::ptr::null()];
            let _ = exec(&shell_c, &args, environ);
            println!("[init] {} exited, trying next shell", shell);
        }
    }

    println!("[init] FATAL: no shell available (/tools/bin/sh, /rescue/sh, /bin/sh all missing)");
    println!("[init] System halted.");
    loop {}
}
