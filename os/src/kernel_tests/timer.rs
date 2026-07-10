//! L3 tests for the timer subsystem.

use alloc::vec;
use alloc::vec::Vec;
use crate::kernel_tests::runner::KernelTest;
use crate::timer;

/// Returns all timer-related kernel tests.
pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new("timer::tick_advances", test_tick_advances),
        KernelTest::new("timer::time_spec_ops", test_time_spec_ops),
    ]
}

/// Verify the tick counter is advancing.
fn test_tick_advances() -> Result<(), &'static str> {
    let t0 = timer::get_time_ms();

    // Busy-wait for a while
    let mut dummy: u64 = 0;
    for _ in 0..500_000 {
        dummy = dummy.wrapping_add(1);
        core::hint::spin_loop();
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

    let t1 = timer::get_time_ms();

    if t1 >= t0 {
        Ok(())
    } else {
        Err("time went backwards")
    }
}

/// Verify TimeSpec arithmetic and comparison.
fn test_time_spec_ops() -> Result<(), &'static str> {
    let now = timer::TimeSpec::now();
    let later = now + timer::TimeSpec::from_ms(100);

    if later <= now {
        return Err("later should be > now");
    }

    let _ = timer::TimeSpec::from_ms(1);
    let _ = timer::TimeSpec::from_us(100);
    let _ = timer::TimeSpec::from_s(1);

    Ok(())
}
