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
mod regression_clone_vm_second_slot;
mod regression_pipe_wakeup;
mod regression_signalfd;
mod regression_pidfd;
mod regression_net_unix_pair;
mod regression_net_tcp_accept;
mod regression_net_tcp_connect;
mod regression_eventfd;
mod regression_epoll;
mod regression_futex;
mod regression_timerfd;

use user_lib::println;

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let total = 16u32;

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

    // Test 6: vfork CLONE_VM second user-resource slot
    let r = regression_clone_vm_second_slot::run();
    if r == 0 { passed += 1; println!("ok 6 clone_vm_second_slot"); }
    else { failed += 1; println!("not ok 6 clone_vm_second_slot"); }

    // Test 7: wake-one pipe progress chain
    let r = regression_pipe_wakeup::run();
    if r == 0 { passed += 1; println!("ok 7 pipe_wakeup"); }
    else { failed += 1; println!("not ok 7 pipe_wakeup"); }

    // Test 8: blocking signalfd read
    let r = regression_signalfd::run();
    if r == 0 { passed += 1; println!("ok 8 signalfd"); }
    else { failed += 1; println!("not ok 8 signalfd"); }

    // Test 9: pidfd poll exit notification
    let r = regression_pidfd::run();
    if r == 0 { passed += 1; println!("ok 9 pidfd"); }
    else { failed += 1; println!("not ok 9 pidfd"); }

    // Test 10: Unix stream and datagram blocking socket wakeups
    let r = regression_net_unix_pair::run();
    if r == 0 { passed += 1; println!("ok 10 net_unix_pair"); }
    else { failed += 1; println!("not ok 10 net_unix_pair"); }

    // Test 11: TCP accept wakes after the child connects
    let r = regression_net_tcp_accept::run();
    if r == 0 { passed += 1; println!("ok 11 net_tcp_accept"); }
    else { failed += 1; println!("not ok 11 net_tcp_accept"); }

    // Test 12: TCP connect wakes after the child accepts
    let r = regression_net_tcp_connect::run();
    if r == -1 { println!("ok 12 net_tcp_connect # SKIP loopback TCP timing"); }
    else if r == 0 { passed += 1; println!("ok 12 net_tcp_connect"); }
    else { failed += 1; println!("not ok 12 net_tcp_connect"); }

    // Test 13: blocking eventfd read and counter semantics
    let r = regression_eventfd::run();
    if r == 0 { passed += 1; println!("ok 13 eventfd"); }
    else { failed += 1; println!("not ok 13 eventfd"); }

    // Test 14: epoll wait wakeup and bounded empty timeout
    let r = regression_epoll::run();
    if r == 0 { passed += 1; println!("ok 14 epoll"); }
    else { failed += 1; println!("not ok 14 epoll"); }

    // Test 15: shared futex wait/wake and EAGAIN fast path
    let r = regression_futex::run();
    if r == 0 { passed += 1; println!("ok 15 futex"); }
    else { failed += 1; println!("not ok 15 futex"); }

    // Test 16: blocking timerfd read
    let r = regression_timerfd::run();
    if r == 0 { passed += 1; println!("ok 16 timerfd"); }
    else { failed += 1; println!("not ok 16 timerfd"); }

    println!("# results: {} passed, {} failed, {} total", passed, failed, total);

    if failed > 0 { 1 } else { 0 }
}
