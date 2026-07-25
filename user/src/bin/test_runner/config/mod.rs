extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub mod parse;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum RunMode { Run, Shell, RunThenShell, DriftWindow, Regression }
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum LtpLibc { Musl, Glibc, Both }
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum LtpRunner { Script, Inline, Suite }

pub struct RuntimeConfig {
    pub mode: RunMode, pub mask: u16, pub order: Vec<usize>, pub timeouts: [u64; 13],
    pub ltp_exclude: Vec<String>, pub ltp_include: Vec<String>, pub ltp_from: Option<String>,
    pub ltp_libc: LtpLibc, pub ltp_runner: LtpRunner, pub ltp_suites: Vec<String>,
    pub conf_source: Option<Vec<u8>>, pub diag: bool, pub timer_smoke: bool,
    pub ext4_profile: bool, pub reclaim_profile: bool, pub skip_apk: bool,
    pub drift_windows: u64, pub drift_libc: String, pub drift_pre_mask: u16, pub drift_measure: String,
}

impl RuntimeConfig {
    pub fn default() -> Self {
        Self {
            mode: RunMode::Run, mask: 0x0800,
            order: crate::runner::groups::catalog::default_order(),
            timeouts: crate::runner::groups::catalog::DEFAULT_TIMEOUTS,
            ltp_exclude: crate::runner::ltp::policy::defaults::DEFAULT_LTP_EXCLUDE.iter().map(|v| String::from(*v)).collect(),
            ltp_include: Vec::new(), ltp_from: None, ltp_libc: LtpLibc::Glibc, ltp_runner: LtpRunner::Suite,
            ltp_suites: alloc::vec![String::from("fs"), String::from("fs_perms_simple")], conf_source: None, diag: false, timer_smoke: false, ext4_profile: false,
            reclaim_profile: false, skip_apk: false, drift_windows: 6, drift_libc: String::from("both"),
            drift_pre_mask: 0, drift_measure: String::from("null"),
        }
    }
}

pub fn environment() -> [*const u8; 18] {
    ["SHELL=/bin/bash\0".as_ptr(), "PWD=/\0".as_ptr(), "LOGNAME=root\0".as_ptr(), "MOTD_SHOWN=pam\0".as_ptr(),
     "HOME=/root\0".as_ptr(), "LANG=C.UTF-8\0".as_ptr(), "TERM=vt220\0".as_ptr(), "USER=root\0".as_ptr(),
     "SHLVL=0\0".as_ptr(), "OLDPWD=/root\0".as_ptr(), "PS1=MangoCore:\\w\\$ \0".as_ptr(), "_=/bin/bash\0".as_ptr(),
     "PATH=/:/bin:/sbin:/usr/bin:/usr/sbin\0".as_ptr(), "KCONFIG_PATH=/proc/config\0".as_ptr(),
     "LD_LIBRARY_PATH=/\0".as_ptr(), "LTP_DEV=/dev/vdb2\0".as_ptr(), "LTP_DEV_FS_TYPE=ext4\0".as_ptr(), core::ptr::null()]
}

pub use parse::load_runtime_config;
