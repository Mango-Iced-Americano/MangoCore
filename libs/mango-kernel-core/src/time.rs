//! Pure-logic time types extracted from `os/src/timer.rs`.
//!
//! Contains POSIX TimeSpec/TimeVal structs, arithmetic impls, and constants.
//! Impure parts (hardware clock access, tick conversion) remain in os/src/timer.rs.
//!
//! # Bug fix: TimeSpec::AddAssign normalization
//!
//! The kernel version did NOT normalize `tv_nsec` after addition,
//! so chained `+=` could leave `tv_nsec >= NSEC_PER_SEC`.
//! Fixed here to carry overflow into `tv_sec`.

use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Sub};

// ── Time constants ──────────────────────────────────────────────────────

pub const MSEC_PER_SEC: usize = 1000;

pub const USEC_PER_SEC: usize = 1_000_000;
pub const USEC_PER_MSEC: usize = 1_000;

pub const NSEC_PER_SEC: usize = 1_000_000_000;
pub const NSEC_PER_MSEC: usize = 1_000_000;
pub const NSEC_PER_USEC: usize = 1_000;

// ── Unit type aliases ───────────────────────────────────────────────────

pub type Tick = u64;
pub type Nsec = u64;

// ────────────────────────────────────────────────────────────────────────
//  TimeSpec — POSIX timespec
// ────────────────────────────────────────────────────────────────────────

/// POSIX `timespec`: seconds + nanoseconds.
///
/// # Examples
///
/// ```
/// use mango_kernel_core::time::TimeSpec;
///
/// let t = TimeSpec::from_ms(1500);
/// assert_eq!(t.tv_sec, 1);
/// assert_eq!(t.tv_nsec, 500_000_000);
/// assert_eq!(t.to_ns(), 1_500_000_000);
///
/// let sum = TimeSpec::from_ns(700_000_000) + TimeSpec::from_ns(500_000_000);
/// assert_eq!(sum.tv_sec, 1);
/// assert_eq!(sum.tv_nsec, 200_000_000);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

impl TimeSpec {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            tv_sec: 0,
            tv_nsec: 0,
        }
    }

    #[inline(always)]
    pub fn from_s(s: usize) -> Self {
        Self {
            tv_sec: s,
            tv_nsec: 0,
        }
    }

    #[inline(always)]
    pub fn from_ms(ms: usize) -> Self {
        Self {
            tv_sec: ms / MSEC_PER_SEC,
            tv_nsec: (ms % MSEC_PER_SEC) * NSEC_PER_MSEC,
        }
    }

    #[inline(always)]
    pub fn from_us(us: usize) -> Self {
        Self {
            tv_sec: us / USEC_PER_SEC,
            tv_nsec: (us % USEC_PER_SEC) * NSEC_PER_USEC,
        }
    }

    #[inline(always)]
    pub fn from_ns(ns: usize) -> Self {
        Self {
            tv_sec: ns / NSEC_PER_SEC,
            tv_nsec: ns % NSEC_PER_SEC,
        }
    }

    /// Convert to nanoseconds (may overflow on extreme values).
    #[inline(always)]
    pub fn to_ns(&self) -> usize {
        self.tv_sec * NSEC_PER_SEC + self.tv_nsec
    }

    /// Nanoseconds as u64, saturating on overflow.
    #[inline(always)]
    pub fn to_ns_saturating(&self) -> u64 {
        (self.tv_sec as u64)
            .saturating_mul(NSEC_PER_SEC as u64)
            .saturating_add(self.tv_nsec as u64)
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.tv_sec == 0 && self.tv_nsec == 0
    }
}

// ── TimeSpec ops ────────────────────────────────────────────────────────

/// **BUG FIX**: `add_assign` now normalizes `tv_nsec` so it never exceeds
/// `NSEC_PER_SEC - 1` after repeated accumulation.
impl AddAssign for TimeSpec {
    #[inline(always)]
    fn add_assign(&mut self, rhs: Self) {
        self.tv_sec += rhs.tv_sec;
        self.tv_nsec += rhs.tv_nsec;
        self.tv_sec += self.tv_nsec / NSEC_PER_SEC;
        self.tv_nsec %= NSEC_PER_SEC;
    }
}

impl Add for TimeSpec {
    type Output = Self;

    #[inline(always)]
    fn add(self, other: Self) -> Self {
        let mut sec = self.tv_sec + other.tv_sec;
        let mut nsec = self.tv_nsec + other.tv_nsec;
        sec += nsec / NSEC_PER_SEC;
        nsec %= NSEC_PER_SEC;
        Self {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }
}

impl Sub for TimeSpec {
    type Output = Self;

    #[inline(always)]
    fn sub(self, other: Self) -> Self {
        if self <= other {
            return TimeSpec::new();
        }
        let mut sec = self.tv_sec - other.tv_sec;
        let nsec = if self.tv_nsec >= other.tv_nsec {
            self.tv_nsec - other.tv_nsec
        } else {
            sec -= 1;
            self.tv_nsec + NSEC_PER_SEC - other.tv_nsec
        };
        Self {
            tv_sec: sec,
            tv_nsec: nsec,
        }
    }
}

impl Ord for TimeSpec {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.tv_sec.cmp(&other.tv_sec) {
            Ordering::Less => Ordering::Less,
            Ordering::Equal => self.tv_nsec.cmp(&other.tv_nsec),
            Ordering::Greater => Ordering::Greater,
        }
    }
}

impl PartialOrd for TimeSpec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ────────────────────────────────────────────────────────────────────────
//  TimeVal — POSIX timeval
// ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TimeVal {
    pub tv_sec: usize,
    pub tv_usec: usize,
}

impl TimeVal {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            tv_sec: 0,
            tv_usec: 0,
        }
    }

    #[inline(always)]
    pub fn from_s(s: usize) -> Self {
        Self {
            tv_sec: s,
            tv_usec: 0,
        }
    }

    #[inline(always)]
    pub fn from_ms(ms: usize) -> Self {
        Self {
            tv_sec: ms / MSEC_PER_SEC,
            tv_usec: (ms % MSEC_PER_SEC) * USEC_PER_MSEC,
        }
    }

    #[inline(always)]
    pub fn from_us(us: usize) -> Self {
        Self {
            tv_sec: us / USEC_PER_SEC,
            tv_usec: us % USEC_PER_SEC,
        }
    }

    #[inline(always)]
    pub fn to_us(&self) -> usize {
        self.tv_sec * USEC_PER_SEC + self.tv_usec
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.tv_sec == 0 && self.tv_usec == 0
    }
}

// ── TimeVal ops ─────────────────────────────────────────────────────────

impl Add for TimeVal {
    type Output = Self;

    #[inline(always)]
    fn add(self, other: Self) -> Self {
        let mut sec = self.tv_sec + other.tv_sec;
        let mut usec = self.tv_usec + other.tv_usec;
        sec += usec / USEC_PER_SEC;
        usec %= USEC_PER_SEC;
        Self {
            tv_sec: sec,
            tv_usec: usec,
        }
    }
}

impl Sub for TimeVal {
    type Output = Self;

    #[inline(always)]
    fn sub(self, other: Self) -> Self {
        if self <= other {
            return TimeVal::new();
        }
        let mut sec = self.tv_sec - other.tv_sec;
        let usec = if self.tv_usec >= other.tv_usec {
            self.tv_usec - other.tv_usec
        } else {
            sec -= 1;
            self.tv_usec + USEC_PER_SEC - other.tv_usec
        };
        Self {
            tv_sec: sec,
            tv_usec: usec,
        }
    }
}

impl Ord for TimeVal {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.tv_sec.cmp(&other.tv_sec) {
            Ordering::Less => Ordering::Less,
            Ordering::Equal => self.tv_usec.cmp(&other.tv_usec),
            Ordering::Greater => Ordering::Greater,
        }
    }
}

impl PartialOrd for TimeVal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ────────────────────────────────────────────────────────────────────────
//  Misc types
// ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C)]
pub struct TimeZone {
    pub tz_minuteswest: u32,
    pub tz_dsttime: u32,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ITimerVal {
    pub it_interval: TimeVal,
    pub it_value: TimeVal,
}

impl ITimerVal {
    pub fn new() -> Self {
        Self {
            it_interval: TimeVal::new(),
            it_value: TimeVal::new(),
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Times {
    pub tms_utime: usize,
    pub tms_stime: usize,
    pub tms_cutime: usize,
    pub tms_cstime: usize,
}

pub enum TimeRange {
    TimeSpec(TimeSpec),
    TimeVal(TimeVal),
}

// ────────────────────────────────────────────────────────────────────────
//  Tests
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constant sanity ─────────────────────────────────────────────

    #[test]
    fn constant_nsec_per_sec() {
        assert_eq!(NSEC_PER_SEC, 1_000_000_000);
    }

    #[test]
    fn constant_usec_per_msec() {
        assert_eq!(USEC_PER_MSEC, 1_000);
    }

    #[test]
    fn constant_usec_per_sec() {
        assert_eq!(USEC_PER_SEC, 1_000_000);
    }

    #[test]
    fn constant_nsec_per_msec() {
        assert_eq!(NSEC_PER_MSEC, 1_000_000);
    }

    // ── TimeSpec construction ───────────────────────────────────────

    #[test]
    fn timespec_new_is_zero() {
        let ts = TimeSpec::new();
        assert!(ts.is_zero());
        assert_eq!(ts.tv_sec, 0);
        assert_eq!(ts.tv_nsec, 0);
    }

    #[test]
    fn timespec_from_sec() {
        let ts = TimeSpec::from_s(5);
        assert_eq!(ts.tv_sec, 5);
        assert_eq!(ts.tv_nsec, 0);
        assert!(!ts.is_zero());
    }

    #[test]
    fn timespec_from_ms() {
        let ts = TimeSpec::from_ms(1500);
        assert_eq!(ts.tv_sec, 1);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    #[test]
    fn timespec_from_us() {
        let ts = TimeSpec::from_us(2_500_000);
        assert_eq!(ts.tv_sec, 2);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    #[test]
    fn timespec_from_ns_exact_second() {
        let ts = TimeSpec::from_ns(2_000_000_000);
        assert_eq!(ts.tv_sec, 2);
        assert_eq!(ts.tv_nsec, 0);
    }

    #[test]
    fn timespec_from_ns_subsecond() {
        let ts = TimeSpec::from_ns(500_000_000);
        assert_eq!(ts.tv_sec, 0);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    #[test]
    fn timespec_from_ns_large() {
        let ts = TimeSpec::from_ns(3_500_000_000);
        assert_eq!(ts.tv_sec, 3);
        assert_eq!(ts.tv_nsec, 500_000_000);
    }

    // ── TimeSpec to_ns ──────────────────────────────────────────────

    #[test]
    fn timespec_to_ns_basic() {
        let ts = TimeSpec {
            tv_sec: 3,
            tv_nsec: 500_000_000,
        };
        assert_eq!(ts.to_ns(), 3_500_000_000);
    }

    #[test]
    fn timespec_to_ns_saturating() {
        let ts = TimeSpec::from_ns(3_500_000_000);
        assert_eq!(ts.to_ns_saturating(), 3_500_000_000);
    }

    // ── TimeSpec add with carry ─────────────────────────────────────

    #[test]
    fn timespec_add_no_carry() {
        let a = TimeSpec {
            tv_sec: 1,
            tv_nsec: 200_000_000,
        };
        let b = TimeSpec {
            tv_sec: 2,
            tv_nsec: 300_000_000,
        };
        let sum = a + b;
        assert_eq!(sum.tv_sec, 3);
        assert_eq!(sum.tv_nsec, 500_000_000);
    }

    #[test]
    fn timespec_add_with_carry() {
        let a = TimeSpec::from_ns(500_000_000);
        let b = TimeSpec::from_ns(700_000_000);
        let sum = a + b;
        assert_eq!(sum.tv_sec, 1);
        assert_eq!(sum.tv_nsec, 200_000_000);
    }

    #[test]
    fn timespec_add_large_carry() {
        let a = TimeSpec::from_ns(3_000_000_000);
        let b = TimeSpec::from_ns(4_000_000_000);
        let sum = a + b;
        assert_eq!(sum.tv_sec, 7);
        assert_eq!(sum.tv_nsec, 0);
    }

    #[test]
    fn timespec_add_identity() {
        let ts = TimeSpec::from_ns(1_500_000_000);
        let sum = ts + TimeSpec::new();
        assert_eq!(sum, ts);
    }

    // ── TimeSpec AddAssign normalization (bug fix test) ─────────────

    #[test]
    fn timespec_add_assign_normalizes() {
        let mut ts = TimeSpec::from_ns(500_000_000);
        ts += TimeSpec::from_ns(700_000_000);
        assert_eq!(ts.tv_sec, 1);
        assert_eq!(ts.tv_nsec, 200_000_000);
        assert!(ts.tv_nsec < NSEC_PER_SEC);
    }

    #[test]
    fn timespec_add_assign_chain_normalizes() {
        let mut ts = TimeSpec::new();
        // 700ms + 600ms + 800ms = 2100ms = 2s 100ms
        ts += TimeSpec::from_ns(700_000_000);
        ts += TimeSpec::from_ns(600_000_000);
        ts += TimeSpec::from_ns(800_000_000);
        assert_eq!(ts.tv_sec, 2);
        assert_eq!(ts.tv_nsec, 100_000_000);
        assert!(ts.tv_nsec < NSEC_PER_SEC);
    }

    // ── TimeSpec sub ────────────────────────────────────────────────

    #[test]
    fn timespec_sub_basic() {
        let a = TimeSpec::from_s(5);
        let b = TimeSpec::from_s(3);
        assert_eq!(a - b, TimeSpec::from_s(2));
    }

    #[test]
    fn timespec_sub_with_borrow() {
        let a = TimeSpec {
            tv_sec: 3,
            tv_nsec: 200_000_000,
        };
        let b = TimeSpec {
            tv_sec: 2,
            tv_nsec: 800_000_000,
        };
        let diff = a - b;
        assert_eq!(diff.tv_sec, 0);
        assert_eq!(diff.tv_nsec, 400_000_000);
    }

    #[test]
    fn timespec_sub_clamps_to_zero() {
        let a = TimeSpec::from_s(1);
        let b = TimeSpec::from_s(5);
        assert_eq!(a - b, TimeSpec::new());
    }

    #[test]
    fn timespec_sub_equal_clamps_to_zero() {
        let a = TimeSpec::from_s(3);
        assert_eq!(a - a, TimeSpec::new());
    }

    // ── TimeSpec comparison ─────────────────────────────────────────

    #[test]
    fn timespec_ordering_sec() {
        let a = TimeSpec::from_s(2);
        let b = TimeSpec::from_s(1);
        assert!(a > b);
        assert!(b < a);
    }

    #[test]
    fn timespec_ordering_nsec() {
        let a = TimeSpec {
            tv_sec: 0,
            tv_nsec: 900_000_000,
        };
        let b = TimeSpec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        assert!(a > b);
    }

    #[test]
    fn timespec_ordering_equal() {
        let a = TimeSpec::from_ns(1_500_000_000);
        let b = TimeSpec {
            tv_sec: 1,
            tv_nsec: 500_000_000,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn timespec_second_beats_subsecond() {
        // 1.0s > 999ms
        assert!(TimeSpec::from_s(1) > TimeSpec::from_ms(999));
    }

    #[test]
    fn timespec_same_total_is_equal() {
        // 1s == 1,000,000,000 ns
        assert_eq!(TimeSpec::from_s(1), TimeSpec::from_ns(1_000_000_000));
    }

    // ── TimeSpec is_zero ────────────────────────────────────────────

    #[test]
    fn timespec_is_zero_true() {
        assert!(TimeSpec::new().is_zero());
    }

    #[test]
    fn timespec_is_zero_false_on_sec() {
        assert!(!TimeSpec::from_s(1).is_zero());
    }

    #[test]
    fn timespec_is_zero_false_on_nsec() {
        assert!(!TimeSpec::from_ns(1).is_zero());
    }

    // ── TimeVal construction ────────────────────────────────────────

    #[test]
    fn timeval_new_is_zero() {
        let tv = TimeVal::new();
        assert!(tv.is_zero());
        assert_eq!(tv.tv_sec, 0);
        assert_eq!(tv.tv_usec, 0);
    }

    #[test]
    fn timeval_from_sec() {
        let tv = TimeVal::from_s(3);
        assert_eq!(tv.tv_sec, 3);
        assert_eq!(tv.tv_usec, 0);
    }

    #[test]
    fn timeval_from_ms() {
        let tv = TimeVal::from_ms(2500);
        assert_eq!(tv.tv_sec, 2);
        assert_eq!(tv.tv_usec, 500_000);
    }

    #[test]
    fn timeval_from_us() {
        let tv = TimeVal::from_us(3_750_000);
        assert_eq!(tv.tv_sec, 3);
        assert_eq!(tv.tv_usec, 750_000);
    }

    #[test]
    fn timeval_to_us() {
        let tv = TimeVal {
            tv_sec: 2,
            tv_usec: 500_000,
        };
        assert_eq!(tv.to_us(), 2_500_000);
    }

    // ── TimeVal add with carry ──────────────────────────────────────

    #[test]
    fn timeval_add_no_carry() {
        let a = TimeVal {
            tv_sec: 1,
            tv_usec: 200_000,
        };
        let b = TimeVal {
            tv_sec: 2,
            tv_usec: 300_000,
        };
        let sum = a + b;
        assert_eq!(sum.tv_sec, 3);
        assert_eq!(sum.tv_usec, 500_000);
    }

    #[test]
    fn timeval_add_with_carry() {
        let a = TimeVal {
            tv_sec: 0,
            tv_usec: 800_000,
        };
        let b = TimeVal {
            tv_sec: 0,
            tv_usec: 500_000,
        };
        let sum = a + b;
        assert_eq!(sum.tv_sec, 1);
        assert_eq!(sum.tv_usec, 300_000);
    }

    #[test]
    fn timeval_add_large_carry() {
        let a = TimeVal::from_us(3_000_000);
        let b = TimeVal::from_us(4_000_000);
        let sum = a + b;
        assert_eq!(sum.tv_sec, 7);
        assert_eq!(sum.tv_usec, 0);
    }

    // ── TimeVal sub ─────────────────────────────────────────────────

    #[test]
    fn timeval_sub_basic() {
        let a = TimeVal::from_s(5);
        let b = TimeVal::from_s(3);
        assert_eq!(a - b, TimeVal::from_s(2));
    }

    #[test]
    fn timeval_sub_with_borrow() {
        let a = TimeVal {
            tv_sec: 3,
            tv_usec: 200_000,
        };
        let b = TimeVal {
            tv_sec: 2,
            tv_usec: 800_000,
        };
        let diff = a - b;
        assert_eq!(diff.tv_sec, 0);
        assert_eq!(diff.tv_usec, 400_000);
    }

    #[test]
    fn timeval_sub_clamps_to_zero() {
        let a = TimeVal::from_s(1);
        let b = TimeVal::from_s(5);
        assert_eq!(a - b, TimeVal::new());
    }

    // ── TimeVal comparison ──────────────────────────────────────────

    #[test]
    fn timeval_ordering() {
        assert!(TimeVal::from_s(2) > TimeVal::from_s(1));
        assert!(TimeVal::from_us(999_999) < TimeVal::from_s(1));
    }

    // ── ITimerVal ───────────────────────────────────────────────────

    #[test]
    fn itimerval_new_is_zero() {
        let itv = ITimerVal::new();
        assert!(itv.it_interval.is_zero());
        assert!(itv.it_value.is_zero());
    }
}
