//! MangoCore L3 in-kernel self-test framework.
//!
//! # Overview
//!
//! L3 tests run **inside the kernel** in a special boot mode (`mango.mode=ktest`).
//! No user-mode init is started. The test runner executes registered test functions
//! directly in kernel context and outputs results in TAP format.
//!
//! # Usage
//!
//! Build with:
//! ```bash
//! MANGO_CMDLINE="mango.mode=ktest mango.test=waitqueue" make rv64-ktest
//! ```
//!
//! # Adding a new test
//!
//! 1. Add a test function returning `Result<(), &'static str>` in the appropriate module
//! 2. Register it in `all_tests()` in this file

pub mod runner;

#[path = "waitqueue.rs"]
mod kt_waitqueue;
#[path = "timer.rs"]
mod kt_timer;
#[path = "sched.rs"]
mod kt_sched;
#[path = "mm.rs"]
mod kt_mm;

use runner::KernelTest;
use alloc::vec;
use alloc::vec::Vec;

/// Returns all registered kernel tests.
/// Grouped by subsystem for test selection via `mango.test=<group>`.
pub fn all_tests() -> Vec<(&'static str, Vec<KernelTest>)> {
    vec![
        ("waitqueue", kt_waitqueue::tests()),
        ("timer", kt_timer::tests()),
        ("sched", kt_sched::tests()),
        ("mm", kt_mm::tests()),
    ]
}

/// Main entry point for ktest mode. Never returns.
///
/// Called from `rust_main()` when `BootConfig.mode == Ktest`.
pub fn run_from_bootargs(config: &crate::bootargs::BootConfig) -> ! {
    runner::run_tests(config, &all_tests());
    // runner::run_tests will call shutdown() — this is unreachable
    unreachable!()
}
