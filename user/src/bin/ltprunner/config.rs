use alloc::string::String;
use alloc::vec::Vec;
use user_lib::{close, open, println, read, OpenFlags};

use super::filters::{
    DEFAULT_LTP_EXCLUDE, DEFAULT_LTP_EXCLUDE_GLIBC, DEFAULT_LTP_EXCLUDE_MUSL,
    DEFAULT_LTP_EXCLUDE_UNSUPPORTED,
};

#[cfg(target_arch = "loongarch64")]
pub const DEFAULT_CASE_TIMEOUT_SECS: u64 = 150;
#[cfg(not(target_arch = "loongarch64"))]
pub const DEFAULT_CASE_TIMEOUT_SECS: u64 = 150;

pub struct LtpConfig {
    pub ltp_suites: Vec<String>,
    pub ltp_from: String,
    pub ltp_include: Vec<String>,
    pub ltp_exclude: Vec<String>,
    pub case_timeout_secs: u64,
    pub lwext4_perf_log: bool,
}

fn trim_bytes(mut s: &[u8]) -> &[u8] {
    while let Some(b) = s.first() {
        if *b == b' ' || *b == b'\t' || *b == b'\r' {
            s = &s[1..];
        } else {
            break;
        }
    }
    while let Some(b) = s.last() {
        if *b == b' ' || *b == b'\t' || *b == b'\r' {
            s = &s[..s.len() - 1];
        } else {
            break;
        }
    }
    s
}

fn comma_split(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

fn parse_bool_flag(s: &str) -> bool {
    matches!(s.trim(), "1" | "true" | "yes" | "on")
}

fn default_excludes(libc: &str) -> Vec<String> {
    let mut list: Vec<String> = DEFAULT_LTP_EXCLUDE
        .iter()
        .map(|s| String::from(*s))
        .collect();
    list.extend(
        DEFAULT_LTP_EXCLUDE_UNSUPPORTED
            .iter()
            .map(|s| String::from(*s)),
    );
    let libc_defaults: &[&str] = if libc == "musl" {
        DEFAULT_LTP_EXCLUDE_MUSL
    } else if libc == "glibc" {
        DEFAULT_LTP_EXCLUDE_GLIBC
    } else {
        &[]
    };
    list.extend(libc_defaults.iter().map(|s| String::from(*s)));
    list.extend(default_arch_excludes(libc).iter().map(|s| String::from(*s)));
    list
}

#[cfg(target_arch = "riscv64")]
fn default_arch_excludes(libc: &str) -> &[&str] {
    if libc == "musl" {
        super::filters::DEFAULT_LTP_EXCLUDE_RV64_MUSL
    } else if libc == "glibc" {
        super::filters::DEFAULT_LTP_EXCLUDE_RV64_GLIBC
    } else {
        &[]
    }
}

#[cfg(target_arch = "loongarch64")]
fn default_arch_excludes(libc: &str) -> &[&str] {
    if libc == "musl" {
        super::filters::DEFAULT_LTP_EXCLUDE_LA64_MUSL
    } else if libc == "glibc" {
        super::filters::DEFAULT_LTP_EXCLUDE_LA64_GLIBC
    } else {
        &[]
    }
}

pub fn load_conf(path: &str, libc: &str) -> LtpConfig {
    let mut cfg = LtpConfig {
        ltp_suites: Vec::new(),
        ltp_from: String::new(),
        ltp_include: Vec::new(),
        ltp_exclude: default_excludes(libc),
        case_timeout_secs: DEFAULT_CASE_TIMEOUT_SECS,
        lwext4_perf_log: false,
    };
    let mut conf_exclude = Vec::new();
    let mut conf_libc_exclude = Vec::new();
    let mut conf_arch_libc_exclude = Vec::new();
    let mut ltp_exclude_reset = false;

    let fd = open(path, OpenFlags::RDONLY);
    if fd < 0 {
        println!("[ltprunner] cannot open conf {}, using defaults", path);
        return cfg;
    }

    let mut content = Vec::new();
    let mut tmp_buf = [0u8; 512];
    loop {
        let n = read(fd as usize, &mut tmp_buf);
        if n <= 0 {
            break;
        }
        content.extend_from_slice(&tmp_buf[..n as usize]);
    }
    let _ = close(fd as usize);

    for raw_line in content.split(|b| *b == b'\n') {
        let line = trim_bytes(raw_line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let mut eq_pos = None;
        for (idx, ch) in line.iter().enumerate() {
            if *ch == b'=' {
                eq_pos = Some(idx);
                break;
            }
        }
        let Some(eq_pos) = eq_pos else {
            continue;
        };
        let key = trim_bytes(&line[..eq_pos]);
        let val = trim_bytes(&line[eq_pos + 1..]);

        let Ok(val_str) = core::str::from_utf8(val) else {
            continue;
        };

        if key == b"ltp_suites" {
            cfg.ltp_suites = comma_split(val_str);
        } else if key == b"ltp_from" {
            cfg.ltp_from = String::from(val_str.trim());
        } else if key == b"ltp_include" {
            cfg.ltp_include = comma_split(val_str);
        } else if key == b"ltp_exclude_reset" {
            ltp_exclude_reset = parse_bool_flag(val_str);
        } else if key == b"ltp_case_timeout_secs" {
            if let Ok(secs) = val_str.trim().parse::<u64>() {
                cfg.case_timeout_secs = secs.max(1);
            }
        } else if key == b"ltp_lwext4_perf_log" {
            cfg.lwext4_perf_log = parse_bool_flag(val_str);
        } else if key == b"ltp_exclude" {
            conf_exclude = comma_split(val_str);
        } else if key == b"ltp_exclude_musl" && libc == "musl" {
            conf_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_glibc" && libc == "glibc" {
            conf_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_rv64_musl" && cfg!(target_arch = "riscv64") && libc == "musl"
        {
            conf_arch_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_rv64_glibc"
            && cfg!(target_arch = "riscv64")
            && libc == "glibc"
        {
            conf_arch_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_la64_musl"
            && cfg!(target_arch = "loongarch64")
            && libc == "musl"
        {
            conf_arch_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_la64_glibc"
            && cfg!(target_arch = "loongarch64")
            && libc == "glibc"
        {
            conf_arch_libc_exclude.extend(comma_split(val_str));
        }
    }

    if ltp_exclude_reset {
        cfg.ltp_exclude.clear();
    } else {
        cfg.ltp_exclude = default_excludes(libc);
        cfg.ltp_exclude.extend(conf_exclude);
        cfg.ltp_exclude.extend(conf_libc_exclude);
        cfg.ltp_exclude.extend(conf_arch_libc_exclude);
    }
    cfg
}
