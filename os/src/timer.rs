#![allow(unused)]
use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Sub};

pub use crate::hal::{get_clock_freq, get_time};

use core::time::Duration;

// ── Unit type aliases for readability ──
pub type Tick = u64;
pub type Nsec = u64;

pub const MSEC_PER_SEC: usize = 1000;

pub const USEC_PER_SEC: usize = 1_000_000;
pub const USEC_PER_MSEC: usize = 1_000;

pub const NSEC_PER_SEC: usize = 1_000_000_000;
pub const NSEC_PER_MSEC: usize = 1_000_000;
pub const NSEC_PER_USEC: usize = 1_000;

const NSEC_PER_SEC_U64: u64 = 1_000_000_000;
const USEC_PER_SEC_U64: u64 = 1_000_000;
const MSEC_PER_SEC_U64: u64 = 1_000;

// ─────────────────────────────────────────────────────────
//  Safe time-conversion primitives
// ─────────────────────────────────────────────────────────

/// Hardware counter frequency in Hz.
#[inline(always)]
pub fn clock_freq() -> u64 {
    get_clock_freq() as u64
}

/// Raw hardware tick counter.
#[inline(always)]
pub fn raw_ticks() -> u64 {
    get_time() as u64
}

/// Convert ticks → nanoseconds.
///
/// Uses (sec, remainder) decomposition with u128 intermediate
/// multiplication so the product never overflows u64.
#[inline]
pub fn ticks_to_ns(ticks: u64) -> u64 {
    let freq = clock_freq();
    let sec = ticks / freq;
    let rem = ticks % freq;
    sec.saturating_mul(NSEC_PER_SEC_U64)
        .saturating_add(((rem as u128 * NSEC_PER_SEC_U64 as u128) / freq as u128) as u64)
}

/// Convert ticks → microseconds.
///
/// Avoids the old `freq / USEC_PER_SEC` truncation bug (e.g. 12.5 MHz → 12).
#[inline]
pub fn ticks_to_us(ticks: u64) -> u64 {
    let freq = clock_freq();
    let sec = ticks / freq;
    let rem = ticks % freq;
    sec.saturating_mul(USEC_PER_SEC_U64)
        .saturating_add(((rem as u128 * USEC_PER_SEC_U64 as u128) / freq as u128) as u64)
}

/// Convert ticks → milliseconds.
#[inline]
pub fn ticks_to_ms(ticks: u64) -> u64 {
    let freq = clock_freq();
    let sec = ticks / freq;
    let rem = ticks % freq;
    sec.saturating_mul(MSEC_PER_SEC_U64)
        .saturating_add(((rem as u128 * MSEC_PER_SEC_U64 as u128) / freq as u128) as u64)
}

/// Convert nanoseconds → ticks (rounding **up** so deadline ≤ trigger).
///
/// Saturates instead of panicking on overflow.
#[inline]
pub fn ns_to_ticks_ceil(ns: u64) -> u64 {
    let freq = clock_freq();
    let sec = ns / NSEC_PER_SEC_U64;
    let rem_ns = ns % NSEC_PER_SEC_U64;
    // ceil(rem_ns * freq / NSEC_PER_SEC)
    let rem_ticks = ((rem_ns as u128 * freq as u128).saturating_add(NSEC_PER_SEC_U64 as u128 - 1))
        / NSEC_PER_SEC_U64 as u128;
    sec.saturating_mul(freq).saturating_add(rem_ticks as u64)
}

/// Nanoseconds since boot (monotonic, never wraps).
#[inline]
pub fn now_ns() -> u64 {
    ticks_to_ns(raw_ticks())
}

// ─────────────────────────────────────────────────────────
//  Legacy helpers — keep the same signatures for callers
// ─────────────────────────────────────────────────────────

#[inline(always)]
pub fn get_time_sec() -> usize {
    (raw_ticks() / clock_freq()) as usize
}

#[inline(always)]
pub fn get_time_ms() -> usize {
    ticks_to_ms(raw_ticks()) as usize
}

#[inline(always)]
pub fn get_time_us() -> usize {
    ticks_to_us(raw_ticks()) as usize
}

#[inline(always)]
pub fn get_time_ns() -> usize {
    ticks_to_ns(raw_ticks()) as usize
}

pub fn current_time_duration() -> Duration {
    Duration::from_micros(ticks_to_us(raw_ticks()))
}

// ─────────────────────────────────────────────────────────
//  TimeSpec — POSIX timespec
// ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TimeSpec {
    pub tv_sec: usize,
    pub tv_nsec: usize,
}

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

impl TimeSpec {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            tv_sec: 0,
            tv_nsec: 0,
        }
    }

    /// Build from raw tick count — safe, no overflow.
    #[inline(always)]
    pub fn from_tick(tick: usize) -> Self {
        TimeSpec::from_ns(ticks_to_ns(tick as u64) as usize)
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

    /// Convert to nanoseconds.
    ///
    /// For safety-critical paths, prefer [`to_ns_saturating`] which cannot overflow.
    #[inline(always)]
    pub fn to_ns(&self) -> usize {
        self.tv_sec * NSEC_PER_SEC + self.tv_nsec
    }

    /// Nanoseconds as u64, saturating on overflow.
    #[inline(always)]
    pub fn to_ns_saturating(&self) -> u64 {
        (self.tv_sec as u64)
            .saturating_mul(NSEC_PER_SEC_U64)
            .saturating_add(self.tv_nsec as u64)
    }

    /// Convert an absolute TimeSpec deadline to hardware ticks, rounding up.
    #[inline(always)]
    pub fn to_ticks_ceil(&self) -> usize {
        ns_to_ticks_ceil(self.to_ns_saturating()) as usize
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.tv_sec == 0 && self.tv_nsec == 0
    }

    /// Current monotonic time as TimeSpec.
    #[inline(always)]
    pub fn now() -> Self {
        TimeSpec::from_ns(now_ns() as usize)
    }
}

#[inline(always)]
pub fn timespec_to_ticks_ceil(time: TimeSpec) -> usize {
    time.to_ticks_ceil()
}

// ─────────────────────────────────────────────────────────
//  TimeVal — POSIX timeval
// ─────────────────────────────────────────────────────────

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

    /// Build from raw tick count — safe, no precision loss.
    #[inline(always)]
    pub fn from_tick(tick: usize) -> Self {
        TimeVal::from_us(ticks_to_us(tick as u64) as usize)
    }

    #[inline(always)]
    pub fn to_tick(&self) -> usize {
        let freq = get_clock_freq();
        self.tv_sec * freq + self.tv_usec * freq / USEC_PER_SEC
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

    /// Current monotonic time as TimeVal.
    #[inline(always)]
    pub fn now() -> Self {
        TimeVal::from_us(ticks_to_us(raw_ticks()) as usize)
    }
}

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

// ─────────────────────────────────────────────────────────
//  Misc types
// ─────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────
//  Wall-clock / realtime support
// ─────────────────────────────────────────────────────────

use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

const DEFAULT_BOOT_TIME_OFFSET: u64 = 1_798_761_600; // 2027-01-01 00:00:00 UTC

// 硬件计数器统一由 HAL 的 get_time()/get_clock_freq() 提供；这里只保存
// 可并发调整的 realtime offset，不再发布可变的全局时钟源指针。
static BOOT_TIME_OFFSET_NS: AtomicU64 =
    AtomicU64::new(DEFAULT_BOOT_TIME_OFFSET * NSEC_PER_SEC as u64);

pub fn init_time_from_cmdline(cmdline: &str) {
    if let Some(ts) = parse_cmdline_boot_time(cmdline) {
        let uptime = uptime();
        BOOT_TIME_OFFSET_NS.store((ts - uptime) * NSEC_PER_SEC as u64, AtomicOrdering::Relaxed);
    } else {
        panic!("no valid now= timestamp in cmdline");
    }
}

pub fn current_time() -> u64 {
    current_timespec().tv_sec as u64
}

pub fn current_time_safe() -> u64 {
    current_time()
}

/// Current realtime as TimeSpec (monotonic + boot offset).
pub fn current_timespec() -> TimeSpec {
    let offset = BOOT_TIME_OFFSET_NS.load(AtomicOrdering::Relaxed) as usize;
    TimeSpec::from_ns(get_time_ns().saturating_add(offset))
}

/// Current realtime as TimeVal (monotonic + boot offset).
pub fn current_timeval() -> TimeVal {
    let offset_us =
        (BOOT_TIME_OFFSET_NS.load(AtomicOrdering::Relaxed) / NSEC_PER_USEC as u64) as usize;
    TimeVal::from_us(get_time_us().saturating_add(offset_us))
}

pub fn set_current_timespec(target: TimeSpec) {
    let offset = target.to_ns_saturating().saturating_sub(now_ns());
    BOOT_TIME_OFFSET_NS.store(offset, AtomicOrdering::Relaxed);
}

/// Seconds since boot (monotonic).
pub fn uptime() -> u64 {
    (raw_ticks() / clock_freq()) as u64
}

fn parse_cmdline_boot_time(cmdline: &str) -> Option<u64> {
    for part in cmdline.split_whitespace() {
        if let Some(ts) = part.strip_prefix("now=") {
            if let Ok(val) = ts.parse::<u64>() {
                return Some(val);
            }
        }
    }
    None
}
