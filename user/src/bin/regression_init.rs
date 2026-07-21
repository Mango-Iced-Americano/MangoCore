//! MangoCore L4 regression test PID1 wrapper.
//!
//! Minimal init process for regression mode initramfs.
//! Supervises `/regression` and reports its terminal status.

#![no_std]
#![no_main]

extern crate alloc;

use user_lib::{exec, exit, fork, println, shutdown, waitpid};

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[regression_init] starting regression suite");

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
