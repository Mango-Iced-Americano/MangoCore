pub const DEFAULT_LTP_EXCLUDE: &[&str] = &["rt_sigtimedwait01", "timerfd04", "timerfd_settime02"];
pub const DEFAULT_LTP_EXCLUDE_UNSUPPORTED: &[&str] = &[
    "acct01",
    "acct02",
    "acct02_helper",
    "cacheflush01",
    "clock_gettime03",
    "clock_nanosleep03",
    "clone303",
    "eventfd06",
    "fork13",
    "fork14",
    "futex_waitv01",
    "futex_waitv02",
    "futex_waitv03",
    "futex_wake04",
    "get_mempolicy01",
    "get_mempolicy02",
    "kill13",
    "madvise06",
    "madvise07",
    "madvise08",
    "madvise09",
    "madvise11",
    "memfd_create03",
    "memfd_create04",
    "msgctl05",
    "msgstress01",
    "process_madvise01",
    "prctl06",
    "prctl06_execve",
    "prctl07",
    "prctl10",
    "rt_tgsigqueueinfo01",
    "semctl08",
    "set_thread_area01",
    "sgetmask01",
    "signal06",
    "ssetmask01",
    "sysinfo03",
    "timer_create01",
    "timer_create02",
    "userfaultfd01",
    "ustat01",
    "ustat02",
];
pub const DEFAULT_LTP_EXCLUDE_MUSL: &[&str] = &[
    "clone04",
    "profil01",
    "sigtimedwait01",
    "sigwaitinfo01",
    "nice04",
];
pub const DEFAULT_LTP_EXCLUDE_GLIBC: &[&str] = &[];

#[cfg(target_arch = "riscv64")]
pub const DEFAULT_LTP_EXCLUDE_RV64_MUSL: &[&str] =
    &["epoll_create02", "atof01", "fptest01", "fptest02"];
#[cfg(target_arch = "riscv64")]
pub const DEFAULT_LTP_EXCLUDE_RV64_GLIBC: &[&str] = &["nice05"];
#[cfg(target_arch = "loongarch64")]
pub const DEFAULT_LTP_EXCLUDE_LA64_MUSL: &[&str] = &["clone08"];
#[cfg(target_arch = "loongarch64")]
pub const DEFAULT_LTP_EXCLUDE_LA64_GLIBC: &[&str] = &[];
