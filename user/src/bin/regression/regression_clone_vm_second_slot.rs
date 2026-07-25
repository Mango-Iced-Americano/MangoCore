//! Regression: LA64 vfork child must run from a second user-resource slot.
//! Expected: CLONE_VM child enters user mode, exits with status 0, and the
//!           parent reaps that child with the normal zero wait status.

#![no_std]
#![no_main]

use user_lib::{exit, println, vfork, waitpid};

pub fn run() -> i32 {
    println!("[regression_clone_vm_second_slot] start");

    let child = vfork();
    if child < 0 {
        println!("FAIL: vfork returned {}", child);
        return 1;
    }
    if child == 0 {
        exit(0);
    }

    let mut status = -1;
    let reaped = waitpid(child as usize, &mut status);
    if reaped != child {
        println!(
            "FAIL: waitpid returned {} (expected child {})",
            reaped, child
        );
        return 1;
    }
    if status != 0 {
        println!("FAIL: child wait status {} (expected 0)", status);
        return 1;
    }

    println!("[regression_clone_vm_second_slot] PASS");
    0
}
