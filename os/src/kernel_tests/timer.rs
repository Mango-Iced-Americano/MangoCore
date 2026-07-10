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
        KernelTest::new("timer::now_monotonic", test_now_monotonic),
    ]
}

/// Verify the tick counter is strictly advancing over a busy-wait.
fn test_tick_advances() -> Result<(), &'static str> {
    let t0 = timer::get_time_ms();

    // Busy-wait long enough for at least one timer tick to fire.
    let mut dummy: u64 = 0;
    for _ in 0..2_000_000 {
        dummy = dummy.wrapping_add(1);
        core::hint::spin_loop();
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

    let t1 = timer::get_time_ms();

    if t1 > t0 {
        let _ = dummy; // suppress unused warning
        Ok(())
    } else if t1 == t0 {
        Err("time did not advance (t1 == t0)")
    } else {
        Err("time went backwards (t1 < t0)")
    }
}

/// Verify TimeSpec construction, arithmetic, and comparison.
fn test_time_spec_ops() -> Result<(), &'static str> {
    use timer::TimeSpec;

    // ── Construction ───────────────────────────────────────────
    // from_ms → to_ns round-trip
    if TimeSpec::from_ms(100).to_ns() != 100_000_000 {
        return Err("from_ms(100).to_ns() != 100_000_000");
    }
    // from_s → to_ns
    if TimeSpec::from_s(3).to_ns() != 3_000_000_000 {
        return Err("from_s(3).to_ns() != 3_000_000_000");
    }
    // from_us → to_ns
    if TimeSpec::from_us(500).to_ns() != 500_000 {
        return Err("from_us(500).to_ns() != 500_000");
    }

    // ── Addition with carry ────────────────────────────────────
    // 700M ns + 500M ns = 1.2B ns = 1 sec + 200M ns
    let a = TimeSpec::from_ns(700_000_000);
    let b = TimeSpec::from_ns(500_000_000);
    let sum = a + b;
    let expected = TimeSpec::from_s(1) + TimeSpec::from_ns(200_000_000);
    if sum != expected {
        return Err("add with carry: 700M ns + 500M ns != 1s + 200M ns");
    }

    // ── Subtraction ────────────────────────────────────────────
    // basic
    let five = TimeSpec::from_s(5);
    let three = TimeSpec::from_s(3);
    if (five - three) != TimeSpec::from_s(2) {
        return Err("from_s(5) - from_s(3) != from_s(2)");
    }
    // clamp to zero (Sub impl clamps on underflow)
    if (TimeSpec::from_s(1) - TimeSpec::from_s(5)) != TimeSpec::new() {
        return Err("from_s(1) - from_s(5) should clamp to zero");
    }

    // ── Comparison ─────────────────────────────────────────────
    // Equality across units
    if TimeSpec::from_ms(1000) != TimeSpec::from_s(1) {
        return Err("from_ms(1000) != from_s(1)");
    }
    // Strict ordering
    if !(TimeSpec::from_ms(999) < TimeSpec::from_s(1)) {
        return Err("from_ms(999) should be < from_s(1)");
    }
    // now() + offset > now()
    let now = TimeSpec::now();
    if !(now + TimeSpec::from_ms(100) > now) {
        return Err("now + 100ms should be > now");
    }

    // ── Zero check ─────────────────────────────────────────────
    if !TimeSpec::new().is_zero() {
        return Err("TimeSpec::new().is_zero() should be true");
    }
    if TimeSpec::from_ns(1).is_zero() {
        return Err("TimeSpec::from_ns(1).is_zero() should be false");
    }

    Ok(())
}

/// Verify monotonic clock never goes backwards.
fn test_now_monotonic() -> Result<(), &'static str> {
    let t0 = timer::TimeSpec::now();

    // Small busy-wait to let time advance.
    let mut dummy: u64 = 0;
    for _ in 0..100_000 {
        dummy = dummy.wrapping_add(1);
        core::hint::spin_loop();
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

    let t1 = timer::TimeSpec::now();

    if t1 >= t0 {
        let _ = dummy;
        Ok(())
    } else {
        Err("monotonic TimeSpec went backwards")
    }
}
