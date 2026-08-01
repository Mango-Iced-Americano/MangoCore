extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user_lib::{close, open, println, read, OpenFlags};
use super::{LtpLibc, LtpRunner, RunMode, RuntimeConfig};

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t' | b'\r')) { value = &value[1..]; }
    while matches!(value.last(), Some(b' ' | b'\t' | b'\r')) { value = &value[..value.len() - 1]; }
    value
}
fn parse_mask(value: &[u8]) -> Option<u16> {
    let value = core::str::from_utf8(value).ok()?;
    if let Some(value) = value.strip_prefix("0x") { u16::from_str_radix(value, 16).ok() }
    else if let Some(value) = value.strip_prefix("0b") { u16::from_str_radix(value, 2).ok() }
    else { value.parse().ok() }
}
fn parse_list(value: &[u8]) -> Option<Vec<String>> {
    Some(core::str::from_utf8(value).ok()?.split(',').filter(|v| !v.is_empty()).map(String::from).collect())
}
fn apply(data: &[u8], cfg: &mut RuntimeConfig) {
    for line in data.split(|b| *b == b'\n').map(trim_ascii) {
        if line.is_empty() || line[0] == b'#' { continue; }
        let Some(eq) = line.iter().position(|b| *b == b'=') else { continue; };
        let (key, value) = (trim_ascii(&line[..eq]), trim_ascii(&line[eq + 1..]));
        match key {
            b"mode" => match value { b"shell" => cfg.mode = RunMode::Shell, b"run_then_shell" => cfg.mode = RunMode::RunThenShell, b"drift_window" => cfg.mode = RunMode::DriftWindow, b"regression" => cfg.mode = RunMode::Regression, _ => {} },
            b"mask" => if let Some(mask) = parse_mask(value) { cfg.mask = mask; },
            b"ltp_include" => if let Some(list) = parse_list(value) { cfg.ltp_include = list; },
            b"ltp_from" => cfg.ltp_from = core::str::from_utf8(value).ok().filter(|v| !v.is_empty()).map(String::from),
            b"ltp_libc" => match value { b"musl" => cfg.ltp_libc = LtpLibc::Musl, b"glibc" => cfg.ltp_libc = LtpLibc::Glibc, _ => {} },
            b"ltp_runner" => match value { b"inline" => cfg.ltp_runner = LtpRunner::Inline, b"script" => cfg.ltp_runner = LtpRunner::Script, b"suite" => cfg.ltp_runner = LtpRunner::Suite, _ => {} },
            b"diag" => cfg.diag = matches!(value, b"1" | b"true"), b"timer_smoke" => cfg.timer_smoke = matches!(value, b"1" | b"true"),
            b"skip_apk" => cfg.skip_apk = matches!(value, b"1" | b"true"),
            b"drift_pre_mask" => if let Some(mask) = parse_mask(value) { cfg.drift_pre_mask = mask; },
            _ => if let Some(name) = key.strip_prefix(b"timeout_") { if let (Ok(name), Ok(seconds)) = (core::str::from_utf8(name), core::str::from_utf8(value).unwrap_or("").parse()) { if let Some(index) = crate::runner::groups::catalog::TEST_GROUPS.iter().position(|(group, _)| *group == name) { cfg.timeouts[index] = seconds; } } },
        }
    }
}
fn load(path: &str, cfg: &mut RuntimeConfig) -> bool {
    let fd = open(path, OpenFlags::RDONLY); if fd < 0 { return false; }
    let mut content = Vec::new(); let mut chunk = [0; 512];
    loop { let count = read(fd as usize, &mut chunk); if count <= 0 { break; } content.extend_from_slice(&chunk[..count as usize]); }
    let _ = close(fd as usize); if content.is_empty() { false } else { apply(&content, cfg); true }
}
pub fn load_runtime_config() -> RuntimeConfig {
    let mut cfg = RuntimeConfig::default();
    let source = if load("/sdcard/os_test.conf\0", &mut cfg) { "/sdcard/os_test.conf" } else if load("/os_test.conf\0", &mut cfg) { "/os_test.conf" } else if load("/etc/os_test.conf\0", &mut cfg) { "/etc/os_test.conf" } else { "<default>" };
    cfg.conf_source = Some(source.as_bytes().to_vec());
    println!("[initproc] config source={} mode={} mask=0x{:03X} timer_smoke={} skip_apk={}", source, match cfg.mode { RunMode::Run => "run", RunMode::Shell => "shell", RunMode::RunThenShell => "run_then_shell", RunMode::DriftWindow => "drift_window", RunMode::Regression => "regression" }, cfg.mask, cfg.timer_smoke, cfg.skip_apk);
    cfg
}
