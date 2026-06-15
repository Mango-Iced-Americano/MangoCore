#![allow(unused)]
use core::cmp::Ordering;
use core::ops::{Add, AddAssign, Sub};

pub use crate::hal::{get_clock_freq, get_time};

use core::time::Duration;

pub const MSEC_PER_SEC: usize = 1000;

pub const USEC_PER_SEC: usize = 1_000_000;
pub const USEC_PER_MSEC: usize = 1_000;

pub const NSEC_PER_SEC: usize = 1_000_000_000;
pub const NSEC_PER_MSEC: usize = 1_000_000;
pub const NSEC_PER_USEC: usize = 1_000;

/// Return current time measured by seconds.
#[inline(always)]
pub fn get_time_sec() -> usize {
    let i = get_time() / (get_clock_freq());
    //log::info!("[timer.rs] get_time(): {},sec: {}", get_time(), i);
    i
}

/// Return current time measured by ms.
#[inline(always)]
pub fn get_time_ms() -> usize {
    let i = get_time() / (get_clock_freq() / MSEC_PER_SEC);
    //log::info!("[timer.rs] get_time(): {},ms: {}", get_time(), i);
    i
}

/// Return current time measured by us.
#[inline(always)]
pub fn get_time_us() -> usize {
    let i = get_time() / (get_clock_freq() / USEC_PER_SEC);
    //log::info!("[timer.rs] get_time(): {},us: {}", get_time(), i);
    i
}

/// Return current time measured by nano seconds.
#[inline(always)]
pub fn get_time_ns() -> usize {
    let i = get_time() * NSEC_PER_SEC / (get_clock_freq());
    //log::info!("[timer.rs] get_time(): {},ns: {}", get_time(), i);
    i
}

pub fn current_time_duration() -> Duration {
    Duration::from_micros(get_time_us() as u64)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Traditional UNIX timespec structures represent elapsed time, measured by the system clock
/// # *CAUTION*
/// tv_sec & tv_usec should be usize.
#[repr(C)]
pub struct TimeSpec {
    /// The tv_sec member represents the elapsed time, in whole seconds.
    pub tv_sec: usize,
    /// The tv_usec member captures rest of the elapsed time, represented as the number of microseconds.
    pub tv_nsec: usize,
}
impl AddAssign for TimeSpec {
    #[inline(always)]
    fn add_assign(&mut self, rhs: Self) {
        self.tv_sec += rhs.tv_sec;
        self.tv_nsec += rhs.tv_nsec;
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
    #[inline(always)]
    pub fn from_tick(tick: usize) -> Self {
        let freq = get_clock_freq();
        Self {
            tv_sec: tick / freq,
            tv_nsec: (tick % freq) * NSEC_PER_SEC / freq,
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
    #[inline(always)]
    pub fn to_ns(&self) -> usize {
        self.tv_sec * NSEC_PER_SEC + self.tv_nsec
    }
    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.tv_sec == 0 && self.tv_nsec == 0
    }
    #[inline(always)]
    pub fn now() -> Self {
        TimeSpec::from_tick(get_time())
    }
}

/// Traditional UNIX timeval structures represent elapsed time, measured by the system clock
/// # *CAUTION*
/// tv_sec & tv_usec should be usize.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TimeVal {
    /// The `tv_sec` member represents the elapsed time, in whole seconds
    pub tv_sec: usize,
    /// The `tv_nsec` member represents the rest of the elapsed time in nanoseconds.
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
    pub fn from_tick(tick: usize) -> Self {
        let freq = get_clock_freq();
        Self {
            tv_sec: tick / freq,
            tv_usec: (tick % freq) * USEC_PER_SEC / freq,
        }
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
    #[inline(always)]
    pub fn now() -> Self {
        TimeVal::from_tick(get_time())
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
/// Store the current process times used in the `time()`.
#[repr(C)]
pub struct Times {
    /// user time
    pub tms_utime: usize,
    /// system time
    pub tms_stime: usize,
    /// user time of children
    pub tms_cutime: usize,
    /// system time of children
    pub tms_cstime: usize,
}

pub enum TimeRange {
    TimeSpec(TimeSpec),
    TimeVal(TimeVal),
}

use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// 启动以来的时间源（秒），例如通过读取 mtime / TSC
pub trait TimeSource {
    /// 返回开机以来的时间（单位：秒）
    fn uptime(&self) -> u64;
}

/// 无 RTC/cmdline 时间源时的默认 Unix 启动时间。
///
/// 测试镜像里的 ext4 inode 带有构建时的真实 Unix 时间戳；如果内核把
/// CLOCK_REALTIME 暴露为 1970 起步的开机时间，libc `stat` 会看到文件时间
/// “来自未来”。Linux 的做法是用 RTC/boot time offset 区分 wall clock 与
/// monotonic clock；这里先提供一个稳定 fallback，后续可替换为真实 RTC 初始化。
const DEFAULT_BOOT_TIME_OFFSET: u64 = 1_798_761_600; // 2027-01-01 00:00:00 UTC

/// 系统启动时间偏移（纳秒，使 current_time = Unix 时间）。
static BOOT_TIME_OFFSET_NS: AtomicU64 =
    AtomicU64::new(DEFAULT_BOOT_TIME_OFFSET * NSEC_PER_SEC as u64);

static mut TIME_SOURCE: Option<&'static dyn TimeSource> = None;

/// 注册全局时间源
pub fn init_time_source(ts: &'static dyn TimeSource) {
    unsafe {
        TIME_SOURCE = Some(ts);
    }
}

/// 从引导参数（cmdline）中提取 `now=` 时间戳（Unix 时间）
pub fn init_time_from_cmdline(cmdline: &str) {
    if let Some(ts) = parse_cmdline_boot_time(cmdline) {
        let uptime = uptime();
        BOOT_TIME_OFFSET_NS.store((ts - uptime) * NSEC_PER_SEC as u64, AtomicOrdering::Relaxed);
    } else {
        panic!("no valid now= timestamp in cmdline");
    }
}

/// 当前 Unix 时间戳
pub fn current_time() -> u64 {
    current_timespec().tv_sec as u64
}

/// 当前 Unix 时间戳（安全版本）
pub fn current_time_safe() -> u64 {
    current_time()
}

/// 当前 Unix 时间戳，timespec 形式。
pub fn current_timespec() -> TimeSpec {
    let offset = BOOT_TIME_OFFSET_NS.load(AtomicOrdering::Relaxed) as usize;
    TimeSpec::from_ns(get_time_ns() + offset)
}

/// 当前 Unix 时间戳，timeval 形式。
pub fn current_timeval() -> TimeVal {
    let offset_us =
        (BOOT_TIME_OFFSET_NS.load(AtomicOrdering::Relaxed) / NSEC_PER_USEC as u64) as usize;
    TimeVal::from_us(get_time_us() + offset_us)
}

/// 设置当前 Unix 时间。单调时钟不受影响，仅调整 wall-clock 偏移。
pub fn set_current_timespec(target: TimeSpec) {
    let offset = target.to_ns().saturating_sub(get_time_ns());
    BOOT_TIME_OFFSET_NS.store(offset as u64, AtomicOrdering::Relaxed);
}

/// 获取系统启动以来的时间（秒）
/// 直接使用 HAL 层 get_time() / get_clock_freq()，不依赖 TimeSource 初始化
pub fn uptime() -> u64 {
    get_time_sec() as u64
}

/// 解析启动参数，如 `now=1749900000`
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


const MTIME: *const u64 = 0x0200_BFF8 as *const u64; // RISC-V virt machine 默认 MTIME 地址

pub struct MTime;

impl TimeSource for MTime {
    fn uptime(&self) -> u64 {
        unsafe { core::ptr::read_volatile(MTIME) / 100_0000 } // 100万tick = 1秒
    }
}
