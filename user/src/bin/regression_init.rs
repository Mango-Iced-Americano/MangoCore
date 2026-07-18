//! MangoCore L4 regression test PID1 wrapper.
//!
//! Minimal init process for regression mode initramfs.
//! Runs `/regression`, emits a machine-readable PASS/FAIL marker, then shuts
//! QEMU down so the Makefile gate can distinguish completion from a timeout.

#![no_std]
#![no_main]

extern crate alloc;

use user_lib::{exec, exit, fork, println, shutdown, waitpid};

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[regression_init] starting regression suite");

    let prog = "/regression\0";
    let args: [*const u8; 2] = [prog.as_ptr(), core::ptr::null()];
    let envp: [*const u8; 1] = [core::ptr::null()];

    let pid = fork();
    if pid == 0 {
        let ret = exec(prog, &args, &envp);
        println!("[regression_init] exec failed, errno={}", ret);
        exit(127);
    }
    if pid < 0 {
        println!("[regression_init] fork failed, errno={}", pid);
        println!("[L4 REGRESSION RESULT: FAIL] exit_code=127");
        shutdown();
        return 127;
    }

    let mut status = 0i32;
    let waited = waitpid(pid as usize, &mut status);
    let exit_code = if waited == pid { (status >> 8) & 0xff } else { 127 };
    if exit_code == 0 {
        println!("[L4 REGRESSION RESULT: PASS]");
    } else {
        println!("[L4 REGRESSION RESULT: FAIL] exit_code={}", exit_code);
    }
    shutdown();
    exit_code
}
