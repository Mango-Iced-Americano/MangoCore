//! MangoCore L4 user-mode regression test suite.
//!
//! Each historical bug gets a permanent regression test here.
//! Run via: `regression` binary in MangoCore user space.

#![no_std]
#![no_main]

extern crate alloc;

mod regression_usercopy_pipe;
mod regression_mmap_edge_cases;
mod regression_timer_realtime_jump;
mod regression_rename_long_name;
mod regression_lwext4_truncate_hole;

use user_lib::println;

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = 5u32;

    println!("TAP version 13");
    println!("1..{}", total);

    // Test 1: usercopy pipe
    let r = regression_usercopy_pipe::run();
    if r == 0 { passed += 1; println!("ok 1 usercopy_pipe"); }
    else { failed += 1; println!("not ok 1 usercopy_pipe"); }

    // Test 2: mmap edge cases
    let r = regression_mmap_edge_cases::run();
    if r == 0 { passed += 1; println!("ok 2 mmap_edge_cases"); }
    else { failed += 1; println!("not ok 2 mmap_edge_cases"); }

    // Test 3: timer realtime jump
    let r = regression_timer_realtime_jump::run();
    if r == 0 { passed += 1; println!("ok 3 timer_realtime_jump"); }
    else { failed += 1; println!("not ok 3 timer_realtime_jump"); }

    // Test 4: rename long name
    let r = regression_rename_long_name::run();
    if r == 0 { passed += 1; println!("ok 4 rename_long_name"); }
    else { failed += 1; println!("not ok 4 rename_long_name"); }

    // Test 5: lwext4 truncate hole cold reopen
    let r = regression_lwext4_truncate_hole::run();
    if r == 0 { passed += 1; println!("ok 5 lwext4_truncate_hole"); }
    else { failed += 1; println!("not ok 5 lwext4_truncate_hole"); }

    println!("# results: {} passed, {} failed, {} total", passed, failed, total);

    if failed > 0 { 1 } else { 0 }
}
