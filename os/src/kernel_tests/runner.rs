//! L3 test runner with TAP output, timeout, repeat, and failfast support.
//!
//! Output is color-coded using ANSI escape sequences for readability:
//! - Green for passing tests
//! - Red for failures
//! - Yellow for warnings
//!
//! A machine-parseable result marker is printed before shutdown for CI integration.

use alloc::vec;
use alloc::vec::Vec;
use crate::bootargs::BootConfig;
use crate::hal;
use crate::timer;

// ── ANSI color constants ────────────────────────────────────────
const COLOR_GREEN: &str = "\x1b[32m";
const COLOR_RED: &str = "\x1b[31m";
const COLOR_YELLOW: &str = "\x1b[33m";
const COLOR_RESET: &str = "\x1b[0m";

/// A single kernel test case.
pub struct KernelTest {
    /// Human-readable name, e.g. `"waitqueue::wake_once"`.
    pub name: &'static str,
    /// Test function. Returns `Ok(())` on pass, `Err(reason)` on failure.
    pub func: fn() -> Result<(), &'static str>,
    /// Per-test timeout in milliseconds (0 = use global default).
    pub timeout_ms: usize,
}

impl KernelTest {
    pub const fn new(name: &'static str, func: fn() -> Result<(), &'static str>) -> Self {
        Self {
            name,
            func,
            timeout_ms: 0, // use global default
        }
    }

    pub const fn with_timeout(
        name: &'static str,
        func: fn() -> Result<(), &'static str>,
        timeout_ms: usize,
    ) -> Self {
        Self {
            name,
            func,
            timeout_ms,
        }
    }
}

// ─────────────────────────────────────────────────────────
//  Runner
// ─────────────────────────────────────────────────────────

/// Get the architecture name string for diagnostic output.
fn arch_name() -> &'static str {
    #[cfg(feature = "riscv")]
    { "riscv64" }
    #[cfg(feature = "loongarch64")]
    { "loongarch64" }
    #[cfg(not(any(feature = "riscv", feature = "loongarch64")))]
    { "unknown" }
}

/// Test result summary returned by the runner.
pub struct TestResults {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
}

impl TestResults {
    pub fn all_passed(&self) -> bool {
        self.failed == 0 && self.total > 0
    }
}

/// Run selected tests, print TAP output, and **return** results.
///
/// Does NOT call `hal::shutdown()`. The caller decides what to do next.
///
/// # Parameters
/// - `config`: Parsed bootargs with test selection, repeat, timeout, failfast
/// - `test_groups`: All registered tests, grouped by category name
///
/// # Returns
/// A `TestResults` struct with pass/fail/total counts.
pub fn run_tests_return(
    config: &BootConfig,
    test_groups: &[(&str, Vec<KernelTest>)],
) -> TestResults {
    // Collect tests to run based on selection
    let selected: Vec<&KernelTest> = if config.tests.iter().any(|t| t == "all") {
        test_groups
            .iter()
            .flat_map(|(_, tests)| tests.iter())
            .collect()
    } else {
        test_groups
            .iter()
            .filter(|(group, _)| config.tests.iter().any(|t| t == *group))
            .flat_map(|(_, tests)| tests.iter())
            .collect()
    };

    if selected.is_empty() {
        crate::println!("TAP version 13");
        crate::println!("1..0");
        crate::println!("# No tests selected. Available groups:");
        for (name, tests) in test_groups {
            crate::println!("#   {} ({} tests)", name, tests.len());
        }
        return TestResults {
            passed: 0,
            failed: 0,
            total: 0,
        };
    }

    let total_tests = selected.len() * config.repeat;
    let timeout_ms = if config.timeout_ms > 0 {
        config.timeout_ms
    } else {
        5000 // default 5s
    };

    crate::println!("TAP version 13");
    crate::println!("# arch: {}", arch_name());
    crate::println!("# mode: ktest");
    crate::println!("# repeat: {}", config.repeat);
    crate::println!("# timeout_ms: {}", timeout_ms);
    crate::println!("# failfast: {}", config.failfast);
    crate::println!("1..{}", total_tests);

    let mut test_num: usize = 1;
    let mut passed: usize = 0;
    let mut failed: usize = 0;

    for _rep in 0..config.repeat {
        for test in &selected {
            let per_test_timeout = if test.timeout_ms > 0 {
                test.timeout_ms
            } else {
                timeout_ms
            };

            let start = timer::get_time_ms();
            let result = (test.func)();
            let elapsed = timer::get_time_ms() - start;

            match result {
                Ok(()) => {
                    crate::println!("{}ok{} {} {}", COLOR_GREEN, COLOR_RESET, test_num, test.name);
                    passed += 1;
                }
                Err(reason) => {
                    crate::println!("{}not ok{} {} {}", COLOR_RED, COLOR_RESET, test_num, test.name);
                    crate::println!("  ---");
                    crate::println!("  reason: {}", reason);
                    crate::println!("  elapsed_ms: {}", elapsed);
                    crate::println!("  ...");
                    failed += 1;

                    if config.failfast {
                        crate::println!(
                            "# failfast: stopping after {} passed, {} failed",
                            passed,
                            failed
                        );
                        return TestResults {
                            passed,
                            failed,
                            total: total_tests,
                        };
                    }
                }
            }

            if elapsed > per_test_timeout {
                crate::println!(
                    "{}# WARNING:{} {} took {}ms (timeout={}ms)",
                    COLOR_YELLOW, COLOR_RESET, test.name, elapsed, per_test_timeout
                );
            }

            test_num += 1;
        }
    }

    TestResults {
        passed,
        failed,
        total: total_tests,
    }
}

/// Run selected tests and exit via shutdown.
///
/// # Parameters
/// - `config`: Parsed bootargs with test selection, repeat, timeout, failfast
/// - `test_groups`: All registered tests, grouped by category name
///
/// # Output
/// Prints TAP-compatible output to the console, then calls `hal::shutdown()`.
pub fn run_tests(config: &BootConfig, test_groups: &[(&str, Vec<KernelTest>)]) -> ! {
    let results = run_tests_return(config, test_groups);

    if results.failed > 0 {
        crate::println!(
            "{}# results: {} passed, {} failed, {} total{}",
            COLOR_RED, results.passed, results.failed, results.total, COLOR_RESET
        );
        shutdown_failure();
    } else {
        crate::println!(
            "{}# results: {} passed, {} failed, {} total{}",
            COLOR_GREEN, results.passed, results.failed, results.total, COLOR_RESET
        );
        shutdown_success();
    }
}

fn shutdown_success() -> ! {
    crate::println!("{}[KTEST RESULT: PASS]{}", COLOR_GREEN, COLOR_RESET);
    crate::println!("# ktest: all tests passed. shutting down.");
    for _ in 0..1000 {
        core::hint::spin_loop();
    }
    hal::shutdown();
}

fn shutdown_failure() -> ! {
    crate::println!("{}[KTEST RESULT: FAIL]{}", COLOR_RED, COLOR_RESET);
    crate::println!("# ktest: tests FAILED. shutting down.");
    for _ in 0..1000 {
        core::hint::spin_loop();
    }
    hal::shutdown();
}
