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
pub mod platform;

#[path = "block_device.rs"]
mod kt_block_device;
#[path = "ext4.rs"]
mod kt_ext4;
#[cfg(all(target_arch = "riscv64", feature = "board_vf2"))]
#[path = "gmac.rs"]
mod kt_gmac;
#[path = "ext4_another/mod.rs"]
mod kt_ext4_another;
#[path = "ext4_another_lifetime.rs"]
mod kt_ext4_another_lifetime;
#[path = "mm.rs"]
mod kt_mm;
#[path = "page_cache/mod.rs"]
mod kt_page_cache;
#[path = "sched.rs"]
mod kt_sched;
#[path = "timer.rs"]
mod kt_timer;
#[path = "waitqueue.rs"]
mod kt_waitqueue;

use alloc::vec;
use alloc::vec::Vec;
use runner::KernelTest;
use spin::Mutex;

/// Global storage for ktest boot config.
///
/// Set by `rust_main()` before spawning the runner task; read by
/// [`run_ktest_entry`] when the runner starts.  This avoids
/// capturing the config in a closure (closures are not `fn()`).
pub static KTEST_BOOT_CONFIG: Mutex<Option<crate::bootargs::BootConfig>> = Mutex::new(None);

/// Returns all registered kernel tests.
/// Grouped by subsystem for test selection via `mango.test=<group>`.
pub fn all_tests() -> Vec<(&'static str, Vec<KernelTest>)> {
    vec![
        ("waitqueue", kt_waitqueue::tests()),
        ("timer", kt_timer::tests()),
        ("sched", kt_sched::tests()),
        ("mm", kt_mm::tests()),
        ("page_cache", kt_page_cache::tests()),
        ("ext4", kt_ext4::tests()),
        #[cfg(all(target_arch = "riscv64", feature = "board_vf2"))]
        ("gmac", kt_gmac::tests()),
        ("ext4_another", kt_ext4_another::tests()),
        ("ext4_another_lifetime", kt_ext4_another_lifetime::tests()),
        ("block_device", kt_block_device::tests()),
        ("platform", platform::tests()),
    ]
}

/// Main entry point for ktest mode (shutdown path). Never returns.
///
/// Called from `rust_main()` when `BootConfig.mode == Ktest`.
pub fn run_from_bootargs(config: &crate::bootargs::BootConfig) -> ! {
    runner::run_tests(config, &all_tests());
    // runner::run_tests will call shutdown() — this is unreachable
    unreachable!()
}

/// Entry function for scheduler-active ktest.  Reads config from the
/// global [`KTEST_BOOT_CONFIG`] static and runs the test suite.
///
/// This is a plain `fn()` so it can be passed to
/// [`crate::task::spawn_ktest_task`].
pub fn run_ktest_entry() {
    let config = KTEST_BOOT_CONFIG
        .lock()
        .take()
        .expect("KTEST_BOOT_CONFIG not set");
    runner::run_tests(&config, &all_tests());
    // run_tests calls hal::shutdown() — never returns
    unreachable!();
}

/// Run ktest from within a scheduled ktest task. Returns without shutting down.
///
/// Used when the scheduler is active — tests run inside a spawned
/// kernel task, and the caller handles shutdown after all tasks exit.
pub fn run_from_bootargs_in_task(config: &crate::bootargs::BootConfig) {
    let results = runner::run_tests_return(config, &all_tests());
    if results.failed > 0 {
        crate::println!(
            "\x1b[31m# results: {} passed, {} failed, {} total\x1b[0m",
            results.passed,
            results.failed,
            results.total,
        );
        crate::println!("\x1b[31m[KTEST RESULT: FAIL]\x1b[0m");
    } else {
        crate::println!(
            "\x1b[32m# results: {} passed, {} failed, {} total\x1b[0m",
            results.passed,
            results.failed,
            results.total,
        );
        crate::println!("\x1b[32m[KTEST RESULT: PASS]\x1b[0m");
    }
}
