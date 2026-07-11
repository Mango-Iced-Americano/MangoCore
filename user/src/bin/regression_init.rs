//! MangoCore L4 regression test PID1 wrapper.
//!
//! Minimal init process for regression mode initramfs.
//! Execs `/regression` — when it exits, kernel handles PID 1 exit.

#![no_std]
#![no_main]

extern crate alloc;

use user_lib::{exec, println};

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("[regression_init] starting regression suite");

    let prog = "/regression";
    let args: [*const u8; 2] = [prog.as_ptr(), core::ptr::null()];

    let ret = exec(prog, &args, &[]);
    println!("[regression_init] exec failed, errno={}", ret);
    println!("[L4 REGRESSION RESULT: FAIL] exit_code=127");
    127
}
