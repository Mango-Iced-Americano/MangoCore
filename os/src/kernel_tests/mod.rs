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

pub mod platform;
pub mod runner;
mod probe;
mod fs_smp_fixture;
pub(crate) mod mem_block;

#[path = "waitqueue.rs"]
mod kt_waitqueue;
#[path = "timer.rs"]
mod kt_timer;
#[path = "sched.rs"]
mod kt_sched;
#[path = "smp.rs"]
mod kt_smp;
#[path = "mm.rs"]
mod kt_mm;
#[path = "ext4.rs"]
mod kt_ext4;
#[path = "fs_smp.rs"]
mod kt_fs_smp;
#[path = "net_smp.rs"]
mod kt_net_smp;
#[cfg(all(target_arch = "riscv64", feature = "block_virt"))]
#[path = "net_irq.rs"]
mod kt_net_irq;
#[cfg(target_arch = "riscv64")]
#[path = "console_irq.rs"]
mod kt_console_irq;
#[path = "fs_fat_smp.rs"]
mod kt_fs_fat_smp;
#[path = "page_cache_sync.rs"]
mod kt_page_cache_sync;
#[path = "ext4_another/mod.rs"]
mod kt_ext4_another;

pub(crate) mod platform_fdt_fixture;
mod platform_fdt_snapshot;
#[cfg(target_arch = "riscv64")]
mod dw_mshc;
mod platform_resources;
mod fdt_resource_alignment;
mod smp_topology;

#[path = "block_device.rs"]
mod kt_block_device;
#[path = "block_publication.rs"]
mod kt_block_publication;
#[cfg(all(target_arch = "riscv64", feature = "gmac_probe"))]
#[path = "gmac.rs"]
mod kt_gmac;
#[path = "ext4_another_lifetime.rs"]
mod kt_ext4_another_lifetime;

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
        ("smp", kt_smp::tests()),
        ("mm", kt_mm::tests()),
        ("ext4", kt_ext4::tests()),
        ("ext4_another_lifetime", kt_ext4_another_lifetime::tests()),
        ("page_cache_sync", kt_page_cache_sync::tests()),
        ("ext4_another", kt_ext4_another::tests()),
        ("fs_smp", kt_fs_smp::tests()),
        ("net_smp", kt_net_smp::tests()),
        #[cfg(all(target_arch = "riscv64", feature = "block_virt"))]
        ("net_irq", kt_net_irq::tests()),
        #[cfg(target_arch = "riscv64")]
        ("console_irq", kt_console_irq::tests()),
        ("fs_fat_smp", kt_fs_fat_smp::tests()),
        #[cfg(all(target_arch = "riscv64", feature = "gmac_probe"))]
        ("gmac", kt_gmac::tests()),
        ("block_device", kt_block_device::tests()),
        ("block_publication", kt_block_publication::tests()),
        ("platform", platform::tests()),
        ("platform_fdt_snapshot", platform_fdt_snapshot::tests()),
        #[cfg(target_arch = "riscv64")]
        ("dw_mshc", dw_mshc::tests()),
        ("platform_resources", platform_resources::tests()),
        ("fdt_resource_alignment", fdt_resource_alignment::tests()),
        ("smp_topology", smp_topology::tests()),
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
            "\x1b[31m# results: {} passed, {} skipped, {} failed, {} total\x1b[0m",
            results.passed, results.skipped, results.failed, results.total,
        );
        crate::println!("\x1b[31m[KTEST RESULT: FAIL]\x1b[0m");
    } else {
        crate::println!(
            "\x1b[32m# results: {} passed, {} skipped, {} failed, {} total\x1b[0m",
            results.passed, results.skipped, results.failed, results.total,
        );
        crate::println!("\x1b[32m[KTEST RESULT: PASS]\x1b[0m");
    }
}
