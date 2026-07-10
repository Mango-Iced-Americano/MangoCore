//! MangoCore L4 regression test PID1 wrapper.
//!
//! Minimal init process for regression mode initramfs.
//! Runs `/regression`, emits PASS/FAIL marker, then shuts down.

#![no_std]
#![no_main]

extern crate alloc;

use user_lib::{exec, exit, fork, println, shutdown, waitpid};

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[regression_init] starting regression suite");

    let prog = "/regression\0";
    let args: [*const u8; 2] = [prog.as_ptr(), core::ptr::null()];

    let pid = fork();
    if pid == 0 {
        // Child: exec regression binary
        exec(prog, &args, &[]);
        println!("[regression_init] exec /regression failed");
        exit(127);
    }

    // Parent: wait for child
    let mut status: i32 = 0;
    waitpid(pid as usize, &mut status);

    let exit_code = (status >> 8) & 0xFF;
    if exit_code == 0 {
        println!("[L4 REGRESSION RESULT: PASS]");
    } else {
        println!(
            "[L4 REGRESSION RESULT: FAIL] exit_code={}",
            exit_code
        );
    }

    shutdown();
    0
}
