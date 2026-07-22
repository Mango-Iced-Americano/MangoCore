#![no_std]
#![no_main]

use user_lib::{exec, exit};

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let init = "/sbin/init\0";
    exec(init, &[init.as_ptr(), core::ptr::null()], &[core::ptr::null()]);
    exit(127);
}
