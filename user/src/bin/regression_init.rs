//! MangoCore L4 regression test PID1 wrapper.
//!
//! Minimal init process for regression mode initramfs.
//! Supervises `/regression` and reports its terminal status.

#![no_std]
#![no_main]

extern crate alloc;

use user_lib::syscall::sys_mkdirat;
use user_lib::{
    close, exec, exit, fork, mount, open, println, shutdown, waitpid, write, OpenFlags,
};

const AT_FDCWD: isize = -100;

fn write_etc_file(path: &str, contents: &[u8]) {
    let fd = open(path, OpenFlags::CREATE | OpenFlags::WRONLY | OpenFlags::TRUNC);
    if fd >= 0 {
        let _ = write(fd as usize, contents);
        let _ = close(fd as usize);
    }
}

fn prepare_regression_etc() {
    let _ = sys_mkdirat(AT_FDCWD, "/etc\0", 0o755);
    let _ = mount("none\0".as_ptr(), "/etc\0".as_ptr(), "tmpfs\0".as_ptr(), 0, 0);
    write_etc_file("/etc/passwd\0", b"root:x:0:0:root:/root:/bin/sh\n");
    write_etc_file("/etc/group\0", b"root:x:0:\n");
    write_etc_file("/etc/hosts\0", b"127.0.0.1 localhost\n");
    write_etc_file("/etc/resolv.conf\0", b"nameserver 10.0.2.3\n");
    write_etc_file("/etc/nsswitch.conf\0", b"passwd: files\ngroup: files\nhosts: files dns\n");
    write_etc_file("/etc/hostname\0", b"mangocore\n");
    write_etc_file("/etc/protocols\0", b"ip 0 IP\ntcp 6 TCP\nudp 17 UDP\n");
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[regression_init] starting regression suite");
    prepare_regression_etc();

    let pid = fork();
    if pid == 0 {
        let prog = "/regression\0";
        let args: [*const u8; 2] = [prog.as_ptr(), core::ptr::null()];
        let envp: [*const u8; 1] = [core::ptr::null()];
        exec(prog, &args, &envp);
        exit(127);
    }

    let mut status = 0;
    let exit_code = if pid > 0 && waitpid(pid as usize, &mut status) == pid {
        if status & 0x7F == 0 {
            (status >> 8) & 0xFF
        } else {
            128 + (status & 0x7F)
        }
    } else {
        127
    };

    if exit_code == 0 {
        println!("[L4 REGRESSION RESULT: PASS]");
    } else {
        println!("[L4 REGRESSION RESULT: FAIL] exit_code={}", exit_code);
    }
    shutdown();
    exit_code
}
