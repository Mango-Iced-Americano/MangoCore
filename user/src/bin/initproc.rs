#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user_lib::{
    chdir, close, exec, exit, fork, getdents64, kill, open, println, read, shutdown, sleep, wait,
    waitpid, waitpid_wnohang, OpenFlags, SIGKILL,
};

// ============================================================
// TEST_GROUPS — 组名与脚本文件名的映射
// 索引 0..11 与 mask 的 bit0..bit11 一一对应
// ⚠️ DEFAULT_TIMEOUTS 的索引与此数组一致
// ============================================================
const TEST_GROUPS: [(&str, &str); 12] = [
    ("basic", "basic_testcode.sh"),
    ("busybox", "busybox_testcode.sh"),
    ("lua", "lua_testcode.sh"),
    ("libctest", "libctest_testcode.sh"),
    ("iozone", "iozone_testcode.sh"),
    ("unixbench", "unixbench_testcode.sh"),
    ("iperf", "iperf_testcode.sh"),
    ("libcbench", "libcbench_testcode.sh"),
    ("lmbench", "lmbench_testcode.sh"),
    ("netperf", "netperf_testcode.sh"),
    ("cyclictest", "cyclictest_testcode.sh"),
    ("ltp", "ltp_testcode.sh"),
];

// ============================================================
// 以下三个常量是比赛时的硬编码默认值。
// 如需调整执行顺序、超时时间或 LTP 排除测例，直接修改此处即可，
// 无需依赖 os_test.conf 注入。os_test.conf 可指定同名项覆盖。
//
// ⚠️ 注意：DEFAULT_TIMEOUTS 的索引与 TEST_GROUPS 数组位置（索引 0..11）
// 一一绑定，与 DEFAULT_ORDER 中各组出现的先后顺序无关！
// 例如 DEFAULT_TIMEOUTS[6] 永远是 iperf 的超时时间，无论 iperf
// 在 DEFAULT_ORDER 中排在第几位。
// ============================================================

/// 默认执行顺序（组名列表，按此顺序依次执行）
const DEFAULT_ORDER: &[&str] = &[
    "basic",
    "busybox",
    "lua",
    "ltp",
    "iperf",
    "netperf",
    "libcbench",
    "lmbench",
    "cyclictest",
    "unixbench",
    "libctest",
    "iozone",
];

/// 每组默认超时（秒），索引 0..11 与 TEST_GROUPS 一一对应
/// 例如 [6]=90 表示 TEST_GROUPS[6] (iperf) 的超时时间为 90 秒
const DEFAULT_TIMEOUTS: [u64; 12] = [
    60,  // [0]  basic
    60,  // [1]  busybox
    60,  // [2]  lua
    120, // [3]  libctest
    60,  // [4]  iozone
    120, // [5]  unixbench
    90,  // [6]  iperf
    60,  // [7]  libcbench
    60,  // [8]  lmbench
    90,  // [9]  netperf
    60,  // [10] cyclictest
    300, // [11] ltp
];

/// LTP 默认排除测例名列表
const DEFAULT_LTP_EXCLUDE: &[&str] = &[
    "access04",
    "fallocate02",
    "fallocate03",
    "fanotify13",
    "fanotify14",
    "fanotify23",
    "fremovexattr01",
    "fsconfig03",
    "fsync04",
    "getresuid01_16",
    "getrusage01",
    "kill02",
    "linkat02",
    "mkdir03",
    "move_mount01",
    "mprotect05",
    "sendmsg01",
    "splice01",
    "statfs01",
    "statx06",
    "statx12",
    "umount01",
    "inode02",
    "hugemmap04",
    "gencos",
    "fanotify20",
    "poll02",
    "preadv203_64",
    "pselect01",
    "pwrite02_64",
    "rename03",
    "rename13",
    "umount03",
    "lftest",
    "genj1",
    "shm_test",
    "fallocate05",
    "fanotify18",
    "ioctl05",
    "flock03",
    "mmap20",
    "mount01",
    "inotify10",
    "preadv03",
    "readv02",
    "sigwaitinfo01",
    "pidns04",
    "waitid08",
    "doio",
    "starvation",
    "cve-2017-17052",
    "select02",
];

/// LTP musl 专属排除测例（额外追加）
const DEFAULT_LTP_EXCLUDE_MUSL: &[&str] = &[];
/// LTP glibc 专属排除测例（额外追加）
const DEFAULT_LTP_EXCLUDE_GLIBC: &[&str] = &[];

fn run_bash_cmd(cmd: &str, environ: &[*const u8]) -> i32 {
    let pid = fork();
    if pid == 0 {
        let shell = "/bash\0";
        let dash_c = "-c\0";
        let mut cmd_buf = String::from(cmd);
        cmd_buf.push('\0');
        let argv = [
            shell.as_ptr(),
            dash_c.as_ptr(),
            cmd_buf.as_ptr(),
            core::ptr::null(),
        ];
        exec(shell, &argv, environ);
        exit(127);
    }
    if pid > 0 {
        let mut code = 0;
        loop {
            // 非阻塞收割孤儿僵尸，避免 stdout 延迟输出
            reap_orphans();
            let ret = waitpid(pid as usize, &mut code);
            if ret == pid as isize || ret < 0 {
                break;
            }
            sleep(10);
        }
        // drain_children();
        return code;
    }
    -1
}

// 非阻塞收割所有僵尸孤儿（WNOHANG = 1）
fn reap_orphans() {
    loop {
        let mut status = 0i32;
        let ret = waitpid_wnohang(-1, &mut status);
        if ret <= 0 {
            break;
        }
    }
}

/// 阻塞等待所有子进程退出并回收（直到 ECHILD）
fn drain_children() {
    // 先非阻塞快速收一轮
    reap_orphans();
    // 再阻塞等待剩余还在运行的子进程
    loop {
        let mut status = 0i32;
        if waitpid(!0, &mut status) < 0 {
            break; // ECHILD — 真的没有子进程了
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum RunMode {
    Run,
    Shell,
    RunThenShell,
}

fn mode_name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Run => "run",
        RunMode::Shell => "shell",
        RunMode::RunThenShell => "run_then_shell",
    }
}

#[derive(Clone)]
struct RuntimeConfig {
    mode: RunMode,
    mask: u16,
    /// 执行顺序：TEST_GROUPS 的索引数组，按此顺序依次执行每组
    order: Vec<usize>,
    /// 每测例超时（秒），索引与 TEST_GROUPS 一一绑定，不与 order 位置绑定
    timeouts: [u64; 12],
    /// LTP 排除测例名列表（musl 和 glibc 共用）
    ltp_exclude: Vec<String>,
    /// LTP musl 专属排除测例
    ltp_exclude_musl: Vec<String>,
    /// LTP glibc 专属排除测例
    ltp_exclude_glibc: Vec<String>,
    /// LTP include 白名单（非空时只跑这些测例）
    ltp_include: Vec<String>,
    /// LTP 起始测例名（不设置则从头开始）
    ltp_from: Option<String>,
    /// LTP 只跑哪个 libc：musl | glibc | both（默认）
    ltp_libc: LtpLibc,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum LtpLibc {
    Musl,
    Glibc,
    Both,
}

impl RuntimeConfig {
    fn default() -> Self {
        let order = DEFAULT_ORDER
            .iter()
            .map(|name| {
                TEST_GROUPS
                    .iter()
                    .position(|(n, _)| n == name)
                    .expect("DEFAULT_ORDER contains unknown group name")
            })
            .collect();
        Self {
            mode: RunMode::Run,
            mask: 0x0fff,
            order,
            timeouts: DEFAULT_TIMEOUTS,
            ltp_exclude: DEFAULT_LTP_EXCLUDE
                .iter()
                .map(|s| String::from(*s))
                .collect(),
            ltp_exclude_musl: DEFAULT_LTP_EXCLUDE_MUSL
                .iter()
                .map(|s| String::from(*s))
                .collect(),
            ltp_exclude_glibc: DEFAULT_LTP_EXCLUDE_GLIBC
                .iter()
                .map(|s| String::from(*s))
                .collect(),
            ltp_include: Vec::new(),
            ltp_from: None,
            ltp_libc: LtpLibc::Both,
        }
    }
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
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

fn parse_mask(bytes: &[u8]) -> Option<u16> {
    let s = core::str::from_utf8(bytes).ok()?;
    if let Some(rest) = s.strip_prefix("0x") {
        u16::from_str_radix(rest, 16).ok()
    } else if let Some(rest) = s.strip_prefix("0b") {
        u16::from_str_radix(rest, 2).ok()
    } else {
        u16::from_str_radix(s, 10).ok()
    }
}

fn parse_mode(bytes: &[u8]) -> Option<RunMode> {
    match bytes {
        b"run" => Some(RunMode::Run),
        b"shell" => Some(RunMode::Shell),
        b"run_then_shell" => Some(RunMode::RunThenShell),
        _ => None,
    }
}

fn parse_order(val: &[u8]) -> Option<Vec<usize>> {
    let s = core::str::from_utf8(val).ok()?;
    let mut indices = Vec::new();
    for name in s.split(',') {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let idx = TEST_GROUPS.iter().position(|(n, _)| *n == name)?;
        indices.push(idx);
    }
    if indices.is_empty() {
        None
    } else {
        Some(indices)
    }
}

fn apply_conf_bytes(data: &[u8], cfg: &mut RuntimeConfig) {
    for raw_line in data.split(|b| *b == b'\n') {
        let line = trim_ascii(raw_line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let mut split_at = None;
        for (idx, ch) in line.iter().enumerate() {
            if *ch == b'=' {
                split_at = Some(idx);
                break;
            }
        }
        let Some(eq_pos) = split_at else {
            continue;
        };
        let key = trim_ascii(&line[..eq_pos]);
        let val = trim_ascii(&line[eq_pos + 1..]);
        if key == b"mode" {
            if let Some(mode) = parse_mode(val) {
                cfg.mode = mode;
            }
        } else if key == b"mask" {
            if let Some(mask) = parse_mask(val) {
                cfg.mask = mask;
            }
        } else if key == b"order" {
            if let Some(order) = parse_order(val) {
                cfg.order = order;
            }
        } else if key.starts_with(b"timeout_") {
            // timeout_xxx=秒，例如 timeout_iperf=90
            let group_name = core::str::from_utf8(&key[b"timeout_".len()..]).ok();
            let val_str = core::str::from_utf8(val).ok();
            if let (Some(name), Some(sec_str)) = (group_name, val_str) {
                if let Some(idx) = TEST_GROUPS.iter().position(|(n, _)| *n == name) {
                    if let Ok(secs) = sec_str.parse::<u64>() {
                        cfg.timeouts[idx] = secs;
                    }
                }
            }
        } else if key == b"ltp_exclude" {
            let s = core::str::from_utf8(val).ok();
            if let Some(s) = s {
                cfg.ltp_exclude = s
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect();
            }
        } else if key == b"ltp_exclude_musl" {
            let s = core::str::from_utf8(val).ok();
            if let Some(s) = s {
                cfg.ltp_exclude_musl = s
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect();
            }
        } else if key == b"ltp_exclude_glibc" {
            let s = core::str::from_utf8(val).ok();
            if let Some(s) = s {
                cfg.ltp_exclude_glibc = s
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect();
            }
        } else if key == b"ltp_include" {
            let s = core::str::from_utf8(val).ok();
            if let Some(s) = s {
                cfg.ltp_include = s
                    .split(',')
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect();
            }
        } else if key == b"ltp_libc" {
            match val {
                b"musl" => cfg.ltp_libc = LtpLibc::Musl,
                b"glibc" => cfg.ltp_libc = LtpLibc::Glibc,
                _ => {}
            }
        } else if key == b"ltp_from" {
            let s = core::str::from_utf8(val).ok();
            if let Some(s) = s {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    cfg.ltp_from = Some(String::from(trimmed));
                }
            }
        }
    }
}

fn load_conf_from(path: &str, cfg: &mut RuntimeConfig) -> bool {
    let fd = open(path, OpenFlags::RDONLY);
    if fd < 0 {
        return false;
    }
    let mut content = Vec::new();
    let mut tmp_buf = [0u8; 512]; // 每次读一小块
    loop {
        let n = read(fd as usize, &mut tmp_buf);
        if n <= 0 {
            break; // 读完了
        }
        content.extend_from_slice(&tmp_buf[..n as usize]);
    }
    let _ = close(fd as usize);

    if content.is_empty() {
        return false;
    }
    apply_conf_bytes(&content, cfg);
    true
}

fn load_runtime_config() -> RuntimeConfig {
    let mut cfg = RuntimeConfig::default();
    let source = if load_conf_from("/os_test.conf\0", &mut cfg) {
        "/os_test.conf"
    } else if load_conf_from("/etc/os_test.conf\0", &mut cfg) {
        "/etc/os_test.conf"
    } else {
        "<default>"
    };
    println!(
        "[initproc] config source={} mode={} mask=0x{:03X}",
        source,
        mode_name(cfg.mode),
        cfg.mask
    );
    println!("[initproc] LTP exclude list: {:?}", cfg.ltp_exclude);
    if !cfg.ltp_include.is_empty() {
        println!("[initproc] LTP include list: {:?}", cfg.ltp_include);
    }
    cfg
}

fn enter_shell(path: &str, environ: &[*const u8]) {
    if fork() == 0 {
        chdir("/\0");
        exec(path, &[path.as_ptr(), core::ptr::null()], environ);
        exit(127);
    } else {
        loop {
            let mut shell_exit_code: i32 = 0;
            let pid = wait(&mut shell_exit_code);
            if pid <= 0 {
                break;
            }
        }
    }
}

/// 检查是否需要固定时长运行（守护进程模式，脚本会立即退出）。
/// 返回 `Some(毫秒)`：固定等待时长；`None`：使用标准超时+强杀。
fn fixed_timer_ms(script: &str) -> Option<u64> {
    if script.contains("iperf") {
        Some(30000) // iperf: 30秒
    } else {
        None
    }
}

/// 在指定目录下运行测试脚本。
fn run_group_in_dir(
    environ: &[*const u8],
    dir: &str,
    group_name: &str,
    script: &str,
    timeout_secs: u64,
) {
    // 构造比赛的 START/END 标记
    let libc_suffix = if dir.contains("musl") {
        "musl"
    } else {
        "glibc"
    };
    // 比赛评测依赖此标记格式，超时 kill 后需自己补打 END
    let group_start_marker = format!(
        "#### OS COMP TEST GROUP START {}-{} ####",
        group_name, libc_suffix
    );
    let group_end_marker = format!(
        "#### OS COMP TEST GROUP END {}-{} ####",
        group_name, libc_suffix
    );

    let pid = fork();
    if pid < 0 {
        println!(
            "[initproc] fork failed for {} in {} ret={}",
            script, dir, pid
        );
        return;
    }
    if pid == 0 {
        // 脚本会自动输出 START 标记，无需 initproc 打印
        // println!("{}", group_start_marker);

        let cd_ret = chdir(dir);
        if cd_ret < 0 {
            println!(
                "[initproc] chdir failed dir={} ret={} when running {}",
                dir, cd_ret, script
            );
            exit(126);
        }

        let mut cmd = String::new();
        cmd.push_str("./");
        cmd.push_str(script);
        cmd.push('\0');
        let shell = "/bash\0";
        let dash_c = "-c\0";
        let argv = [
            shell.as_ptr(),
            dash_c.as_ptr(),
            cmd.as_ptr(),
            core::ptr::null(),
        ];
        exec(shell, &argv, environ);
        println!(
            "[initproc] exec failed for {} in {} via /bash -c",
            script, dir
        );
        exit(127);
    } else if let Some(fixed_ms) = fixed_timer_ms(script) {
        // 特殊测试（iperf / libctest 等），脚本会立即退出。
        // 固定等 fixed_ms 毫秒让测试跑完。
        let fix_secs = fixed_ms / 1000;
        println!(
            "[initproc] fixed timer ({}s) for {} in {}",
            fix_secs, script, dir
        );
        sleep(fixed_ms as usize);
        let mut exit_code: i32 = 0;
        let _ = waitpid(pid as usize, &mut exit_code);
        // 脚本自身会输出 START/END 标记，initproc 不再重复打印
        println!(
            "[initproc] done {} in {} exit_code={}",
            script, dir, exit_code
        );
    } else {
        // parent: 超时循环 + 强杀
        let mut exit_code: i32 = 0;
        let timeout_ms = timeout_secs * 1000;
        let mut elapsed_ms: u64 = 0;
        const POLL_MS: u64 = 100;
        let mut timed_out = false;

        loop {
            let ret = waitpid_wnohang(pid as isize, &mut exit_code);
            if ret == pid {
                // 正常退出
                break;
            }
            if ret < 0 {
                println!("[initproc] pid={} vanished for {} in {}", pid, script, dir);
                break;
            }

            elapsed_ms += POLL_MS;
            if elapsed_ms >= timeout_ms {
                timed_out = true;
                println!(
                    "[initproc] TIMEOUT ({}s) for {} in {}, sending SIGKILL to pid={}",
                    timeout_secs, script, dir, pid
                );
                let _ = kill(pid as usize, SIGKILL);
                let _ = waitpid(pid as usize, &mut exit_code);
                println!("[initproc] killed pid={} for {} in {}", pid, script, dir);
                break;
            }

            sleep(POLL_MS as usize);
        }

        // 清理残留：等待一会让系统回收孤儿进程
        reap_orphans();
        if timed_out {
            // 超时 kill 后脚本来不及输出 END 标记，由 initproc 补打
            println!("{}", group_end_marker);
        }
        println!(
            "[initproc] done {} in {} exit_code={}",
            script, dir, exit_code
        );
    }
}

/// 运行 LTP 测例（内联枚举，不使用 shell 脚本，支持 exclude + from）。
/// 输出格式与官方 ltp_testcode.sh 完全对齐。
fn run_ltp_binaries(
    environ: &[*const u8],
    dir: &str,
    exclude: &[String],
    include: &[String],
    from: Option<&str>,
    timeout_secs: u64,
) {
    let libc_suffix = if dir.contains("musl") {
        "musl"
    } else {
        "glibc"
    };
    let ltp_dir = format!("{}/ltp/testcases/bin", dir.trim_end_matches('\0'));

    let pid = fork();
    if pid < 0 {
        println!("[initproc] fork failed for ltp in {} ret={}", ltp_dir, pid);
        return;
    }
    if pid == 0 {
        // child: chdir 到 ltp 目录
        let cd_ret = chdir(&format!("{}\0", ltp_dir));
        if cd_ret < 0 {
            println!("[initproc] chdir failed for ltp dir={}", ltp_dir);
            exit(126);
        }

        // 打印 START 标记
        println!("#### OS COMP TEST GROUP START ltp-{} ####", libc_suffix);

        // 打开当前目录
        let fd = open(".\0", OpenFlags::RDONLY);
        if fd < 0 {
            println!("[initproc] ltp: cannot open dir {}", ltp_dir);
            println!("#### OS COMP TEST GROUP END ltp-{} ####", libc_suffix);
            exit(0);
        }

        let mut cases = 1;
        let mut found_from = false;
        let mut buf = [0u8; 8192];
        loop {
            let n = getdents64(fd as usize, &mut buf);
            if n <= 0 {
                break;
            }

            let mut off = 0usize;
            while off < n as usize {
                // Linux dirent64 layout (riscv64):
                //   off 0: d_ino (u64)     — 8 bytes
                //   off 8: d_off (i64)     — 8 bytes
                //   off 16: d_reclen (u16)  — 2 bytes
                //   off 18: d_type (u8)     — 1 byte
                //   off 19: d_name          — variable
                if off + 19 > n as usize {
                    break;
                }
                let reclen = u16::from_ne_bytes([buf[off + 16], buf[off + 17]]) as usize;
                if reclen < 19 || reclen == 0 {
                    break;
                }
                let _d_type = buf[off + 18];
                let name_start = off + 19;

                // 找 null 结尾
                let mut name_end = name_start;
                while name_end < buf.len() && buf[name_end] != 0 {
                    name_end += 1;
                }
                let name = core::str::from_utf8(&buf[name_start..name_end]).unwrap_or("");

                if !name.is_empty() && name != "." && name != ".." {
                    // 跳过源文件 (.c .h) 和 shell 脚本 (.sh)
                    if name.ends_with(".c") || name.ends_with(".h") || name.ends_with(".sh") {
                        off += reclen;
                        continue;
                    }
                    // ltp_from 跳过逻辑：没遇到起始测例前全部跳过
                    if let Some(from_case) = from {
                        if !found_from {
                            if name == from_case {
                                found_from = true;
                            } else {
                                println!(
                                    "CASE {}: {} (skip before ltp_from={})",
                                    cases, name, from_case
                                );
                                cases += 1;
                                off += reclen;
                                continue;
                            }
                        }
                    }

                    // include 白名单过滤：非空时只跑列表中的测例
                    if !include.is_empty() && !include.iter().any(|e| e == name) {
                        println!("CASE {}: {} (not in include)", cases, name);
                        cases += 1;
                        off += reclen;
                        continue;
                    }

                    println!("CASE {}: {}", cases, name);
                    cases += 1;
                    println!("RUN LTP CASE {}", name);
                    // 检查排除列表
                    if exclude.iter().any(|e| e == name) {
                        println!("    SKIP (excluded)");
                        println!("FAIL LTP CASE {} : -1 (excluded)", name);
                    } else {
                        let cmd = format!("cd {} && ./{}", ltp_dir, name);
                        let ret = run_bash_cmd(&cmd, environ);
                        println!("FAIL LTP CASE {} : {}", name, ret);
                    }
                }

                off += reclen;
            }
        }

        let _ = close(fd as usize);
        println!("#### OS COMP TEST GROUP END ltp-{} ####", libc_suffix);
        exit(0);
    } else {
        // parent: 超时 + 强杀（与 run_group_in_dir 一致）
        let mut exit_code: i32 = 0;
        let timeout_ms = timeout_secs * 1000;
        let mut elapsed_ms: u64 = 0;
        const POLL_MS: u64 = 100;
        let mut timed_out = false;

        loop {
            let ret = waitpid_wnohang(pid as isize, &mut exit_code);
            if ret == pid {
                break;
            }
            if ret < 0 {
                println!("[initproc] ltp pid={} vanished", pid);
                break;
            }

            elapsed_ms += POLL_MS;
            if elapsed_ms >= timeout_ms {
                timed_out = true;
                println!(
                    "[initproc] TIMEOUT ({}s) for ltp in {}, sending SIGKILL to pid={}",
                    timeout_secs, ltp_dir, pid
                );
                let _ = kill(pid as usize, SIGKILL);
                let _ = waitpid(pid as usize, &mut exit_code);
                println!("[initproc] killed ltp pid={}", pid);
                break;
            }

            sleep(POLL_MS as usize);
        }

        reap_orphans();
        if timed_out {
            println!(
                "#### OS COMP TEST GROUP END ltp-{} ####",
                if dir.contains("musl") {
                    "musl"
                } else {
                    "glibc"
                }
            );
        }
        println!("[initproc] done ltp in {} exit_code={}", dir, exit_code);
    }
}

fn run_selected_groups(environ: &[*const u8], cfg: &RuntimeConfig) {
    println!(
        "[initproc] run_selected_groups start mask=0x{:03X} order={:?}",
        cfg.mask,
        cfg.order
            .iter()
            .map(|i| TEST_GROUPS[*i].0)
            .collect::<Vec<_>>()
    );
    for &idx in &cfg.order {
        let (group_name, script) = TEST_GROUPS[idx];
        // mask 作为过滤器
        if (cfg.mask & (1u16 << idx as u16)) == 0 {
            println!("[initproc] skip {} (mask bit{} not set)", group_name, idx);
            continue;
        }
        let timeout_secs = cfg.timeouts[idx];
        println!(
            "[initproc] select group={} timeout={}s",
            group_name, timeout_secs
        );
        if group_name == "ltp" {
            // LTP 使用内联枚举（支持 exclude + from），不走 shell 脚本
            let libc = cfg.ltp_libc;
            if libc == LtpLibc::Musl || libc == LtpLibc::Both {
                let exclude_musl: Vec<String> = cfg
                    .ltp_exclude
                    .iter()
                    .chain(&cfg.ltp_exclude_musl)
                    .cloned()
                    .collect();
                run_ltp_binaries(
                    environ,
                    "/musl\0",
                    &exclude_musl,
                    &cfg.ltp_include,
                    cfg.ltp_from.as_deref(),
                    timeout_secs,
                );
            }
            if libc == LtpLibc::Glibc || libc == LtpLibc::Both {
                let exclude_glibc: Vec<String> = cfg
                    .ltp_exclude
                    .iter()
                    .chain(&cfg.ltp_exclude_glibc)
                    .cloned()
                    .collect();
                run_ltp_binaries(
                    environ,
                    "/glibc\0",
                    &exclude_glibc,
                    &cfg.ltp_include,
                    cfg.ltp_from.as_deref(),
                    timeout_secs,
                );
            }
        } else {
            run_group_in_dir(environ, "/musl\0", group_name, script, timeout_secs);
            run_group_in_dir(environ, "/glibc\0", group_name, script, timeout_secs);
        }
        // 每组之间休息一会，清理孤儿进程、让网络连接完全关闭
        println!("[initproc] sleep 1s before next group");
        sleep(1000);
    }
    println!("[initproc] run_selected_groups done");
}

fn run_unix_standalone_tests(environ: &[*const u8]) {
    // 独立的 Unix Domain Socket 测试程序（完全不依赖 LTP 框架）
    // 编译自 user/src/bin/unix_test.rs
    let testdir = "/";
    let name = "unix_test";
    println!("=== STANDALONE UNIX TEST: {} ===", name);
    let cmd = format!("cd {} && ./{}", testdir, name);
    let ret = run_bash_cmd(&cmd, environ);
    println!("=== STANDALONE UNIX TEST: {} exit={} ===", name, ret);
    println!(
        "[initproc] standalone unix test '{}' returned {}",
        name, ret
    );
}

fn run_ltp_network_tests(environ: &[*const u8], exclude: &[String]) {
    // LTP testcases/bin 中与网络/Socket 相关的独立 ELF 测例。
    // 分为多个子列表，按功能分类。
    // 注意：部分测例可能因内核缺少对应功能而返回 TCONF（跳过），属正常行为。

    // ============ 1. Socket 系统调用基础 ============
    let socket_syscall_cases = [
        "socket01",     // socket() 系统调用基础
        "socket02",     // socket() with SOCK_CLOEXEC/SOCK_NONBLOCK
        "socketpair01", // socketpair() 基础
        "socketpair02", // socketpair() with close-on-exec/nonblock
        "socketcall01", // socketcall(2) raw syscall 基础 (TCP/UDP/RAW/UNIX)
        "socketcall02", // socketcall(2) 错误测试
        "socketcall03", // socketcall(2) bind+listen 测试
        "bind01",
        "bind02",
        "bind03",
        "bind04",
        "bind05",
        "bind06",
        "connect01",
        "connect02",
        "listen01",
        "accept01",
        "accept02",
        "accept03",
        "accept4_01",
        "shutdown01", // shutdown() SHUT_RD/SHUT_WR/SHUT_RDWR
        "shutdown02", // shutdown() 错误测试
    ];

    // ============ 2. 数据收发 ============
    let data_io_cases = [
        "send01",
        "send02",
        "sendto01",
        "sendto02",
        "sendto03",
        "sendmsg01",
        "sendmsg02",
        "sendmsg03",
        "sendmmsg01",
        "sendmmsg02",
        "recv01",
        "recvfrom01",
        "recvmsg01",
        "recvmsg02",
        "recvmsg03",
        "recvmmsg01",
        "sendfile01",
        "sendfile02",
        "sendfile03",
        "sendfile04",
        "sendfile05",
        "sendfile06",
        "sendfile07",
        "sendfile08",
        "sendfile09",
    ];

    // ============ 3. Socket 选项 / 名称 ============
    let socket_opt_cases = [
        "getsockname01",
        "getpeername01",
        "getsockopt01",
        "getsockopt02",
        "setsockopt01",
        "setsockopt02",
        "setsockopt03",
        "setsockopt04",
        "setsockopt05",
        "setsockopt06",
        "setsockopt07",
        "sockioctl01", // socket ioctl 测试
    ];

    // ============ 4. 网络工具 / 诊断 ============
    let net_tool_cases = [
        "add_ipv6addr",
        "check_icmpv4_connectivity",
        "check_icmpv6_connectivity",
        "vsock01", // AF_VSOCK 测试
    ];

    // ============ 5. 网络栈高级特性（独立 ELF） ============
    let net_adv_cases = [
        // packet(7) / AF_PACKET
        "fanout01", // AF_PACKET fanout 测试
        // tcp_fastopen
        "tcp_fastopen01", // TCP Fast Open 基础
        // TCP 拥塞控制
        "dctcp01", // DCTCP 拥塞控制
        "bbr01",
        "bbr02", // BBR 拥塞控制
    ];

    // ============ 6. 多路 I/O 复用（与网络密切相关） ============
    let io_multiplex_cases = [
        "poll01",
        "poll02",
        //    "ppoll01",
        "ppoll02",
        "select01",
        "select02",
        "select03",
        "select04",
        "pselect01",
        "pselect02",
        "pselect03",
        "epoll01",
        "epoll02",
        "epoll03",
        "epoll04",
        "epoll05",
        "epoll_ctl01",
        "epoll_wait01",
    ];

    // ============ 7. IPv6 / 地址解析 ============
    let ipv6_cases = [
        "getaddrinfo01",
        "in6_01",
        "in6_02",
        "asapi_01",
        "asapi_02",
        "asapi_03",
    ];

    // ============ 8. Unix Domain Socket 专项测试 ============
    // 以下测例经在 LTP 仓库 (linux-test-project/ltp) 中逐一查证确认存在，
    // 它们专门或主要在 AF_UNIX domain socket 上运行，是验证 Unix socket
    // 实现正确性的核心测试集。
    //
    // 分类说明：
    //   [专用] = 该测例专门为 AF_UNIX 设计（如 bind03 测试 UNIX rebind）
    //   [包含] = 该测例将 PF_UNIX/AF_UNIX 作为 test domain 之一
    //
    //  注意：部分测例已在前面分类中出现，此处单独列出以便聚焦 Unix socket 调试。
    //  重复运行无害（测例均包含完善的 cleanup 逻辑）。
    let unix_socket_cases = [
        // ---- AF_UNIX 专用测例（仅对 Unix socket 有意义） ----
        "bind03",       // [专用] AF_UNIX STREAM rebind → EINVAL
        "bind04",       // [专用] AF_UNIX pathname/abstract stream + seqpacket
        "bind05",       // [专用] AF_UNIX pathname/abstract dgram
        "getsockopt02", // [专用] SO_PEERCRED 获取对端凭证 (AF_UNIX-only)
        "shutdown01",   // [专用] AF_UNIX shutdown SHUT_RD/SHUT_WR/SHUT_RDWR
        // ---- 核心 socket 创建/绑定（包含 PF_UNIX） ----
        "socket01",     // [包含] PF_UNIX SOCK_DGRAM 创建
        "socket02",     // [包含] socket() + SOCK_CLOEXEC/SOCK_NONBLOCK
        "socketpair01", // [包含] PF_UNIX socketpair dgram + stream
        "socketpair02", // [包含] PF_UNIX socketpair + close-on-exec/nonblock
        "socketcall01", // [包含] socketcall raw: unix domain dgram
        // ---- 地址绑定与连接 ----
        "bind01",    // [包含] AF_UNIX sockaddr 绑定到错误 socket (EAFNOSUPPORT)
        "connect01", // [包含] PF_UNIX connect 测试
        "listen01",  // [包含] PF_UNIX listen 测试
        "accept01",  // [包含] PF_UNIX accept 测试
        // ---- 数据收发（包含 PF_UNIX 域） ----
        "send01",     // [包含] PF_UNIX send 测试
        "sendto01",   // [包含] PF_UNIX sendto 测试
        "recv01",     // [包含] PF_UNIX recv 测试
        "recvfrom01", // [包含] PF_UNIX recvfrom 测试
        "sendmsg01",  // [包含] PF_UNIX SOCK_DGRAM sendmsg (rights passing)
        "recvmsg01",  // [包含] AF_UNIX SOCK_STREAM recvmsg
        // ---- socket 选项 / 名称 ----
        "getsockname01", // [包含] PF_UNIX getsockname
        "getpeername01", // [包含] PF_UNIX socketpair getpeername
        "setsockopt01",  // [包含] PF_UNIX setsockopt
        "sockioctl01",   // [包含] PF_UNIX sockioctl
    ];

    // ============ 9. 网络 Shell 脚本（需要网络基础设施，仅尝试） ============
    // let net_shell_cases = [
    //     // busy_poll（Busy Poll 轮询）
    //     // "busy_poll01.sh", "busy_poll02.sh", "busy_poll03.sh",
    //     // iptables 防火墙
    //     // "iptables01.sh",
    //     // nftables
    //     // "nft01.sh",
    //     // MPLS
    //     // "mpls01.sh", "mpls02.sh", "mpls03.sh", "mpls04.sh",
    //     // IP 路由
    //     // "ip_tests.sh",
    //     // 网络命名空间 / 虚拟化
    //     // "ipvlan01.sh",
    //     // MACsec 加密
    //     // "macsec01.sh", "macsec02.sh", "macsec03.sh",
    //     // 隧道协议
    //     // "gre01.sh", "gre02.sh",
    //     // "geneve01.sh", "geneve02.sh",
    //     // "fou01.sh",
    //     // SCTP / DCCP
    //     // "sctp01.sh",
    //     // "dccp01.sh",
    //     // TCP Fast Open shell wrapper
    //     // "tcp_fastopen_run.sh",
    // ];

    // 将所有子列表合并
    let net_cases: Vec<&str> = socket_syscall_cases
        .iter()
        .chain(data_io_cases.iter())
        .chain(socket_opt_cases.iter())
        .chain(net_tool_cases.iter())
        .chain(net_adv_cases.iter())
        // .chain(io_multiplex_cases.iter())
        .chain(ipv6_cases.iter())
        // .chain(unix_socket_cases.iter())
        // .chain(net_shell_cases.iter())
        .copied()
        .collect();

    // let net_cases: Vec<&str> = vec!["getsockopt02"];

    // let net_cases: Vec<&str> = unix_socket_cases.iter().copied().collect();
    let testdir = "/musl/ltp/testcases/bin";

    println!(
        "[initproc] LTP network tests begin ({} cases)",
        net_cases.len()
    );

    for &name in &net_cases {
        if exclude.iter().any(|e| e == name) {
            println!("[initproc] LTP skip (excluded): {}", name);
            continue;
        }
        println!("=== LTP-NET: {} ===", name);
        let cmd = format!("cd {} && ./{}", testdir, name);
        let ret = run_bash_cmd(&cmd, environ);
        println!("=== LTP-NET: {} exit={} ===", name, ret);
        println!("[initproc] LTP network test '{}' returned {}", name, ret);
    }

    println!("[initproc] LTP network tests done");
}

fn run_ltp_signal_tests(environ: &[*const u8], exclude: &[String]) {
    // LTP testcases/bin 中与信号（Signal）处理相关的测例。
    // 目的：作为控制变量，先验证信号系统（SA_RESTART/EINTR/sigprocmask/定时器）
    // 是否正确，再排查网络栈的阻塞/唤醒问题。
    //
    // 测例名核对自 LTP 上游（https://github.com/linux-test-project/ltp）：
    //   - sigaction/ 只有 01, 02（没有 sigaction16）
    //   - sigprocmask/ 只有 01（没有 02）
    //   - pselect/ 有 01/02/03（没有 pselect01_sig）
    //   - 没有 interrupt/ 目录
    let signal_cases = [
        // ---- 核心：sigaction（SA_RESTART + 系统调用重启） ----
        "sigaction01", // 基础 sigaction：设置信号处理器
        "sigaction02", // 测试 SA_RESTART 标志：被中断的 read/write 能否自动重启
        // ---- 基础 signal 函数 ----
        "signal01", // ANSI C signal() 函数基础
        "signal02", // signal() 返回值
        "signal03", // signal() SIG_IGN/SIG_DFL
        "signal04", // signal() 可重入性
        "signal05", // signal() 多次设置
        "signal06", // signal() 综合场景
        // ---- 信号屏蔽字 ----
        "sigprocmask01", // sigprocmask 基础功能：屏蔽/解除屏蔽
        // ---- 未决信号 ----
        "sigpending02", // 测试未决信号集（sigpending 系统调用）
        // ---- 实时信号 ----
        "rt_sigaction01", // rt_sigaction 基础逻辑 + SA_SIGINFO
        "rt_sigaction02", // rt_sigaction 信号掩码继承
        "rt_sigaction03", // rt_sigaction 综合场景
        // ---- 定时器信号 ----
        "setitimer01", // ITIMER_REAL 定时器能否准时产生 SIGALRM
        "setitimer02", // setitimer 边界情况（0 值停止定时器）
        "getitimer01", // 定时器剩余时间计算
        "getitimer02", // getitimer 边界情况
        // ---- clock_getres（setitimer/getitimer 的依赖） ----
        "clock_getres01", // clock_getres 系统调用精度查询
        // ---- pselect 专题 ----
        "pselect01", // 基础 pselect 功能
        "pselect02", // pselect + 信号掩码原子性
        "pselect03", // pselect 超时行为
    ];

    let testdir = "/musl/ltp/testcases/bin";

    println!(
        "[initproc] LTP signal tests begin ({} cases)",
        signal_cases.len()
    );

    for &name in &signal_cases {
        if exclude.iter().any(|e| e == name) {
            println!("[initproc] LTP skip (excluded): {}", name);
            continue;
        }
        println!("=== LTP-SIG: {} ===", name);
        let cmd = format!("cd {} && ./{}", testdir, name);
        let ret = run_bash_cmd(&cmd, environ);
        println!("=== LTP-SIG: {} exit={} ===", name, ret);
        println!("[initproc] LTP signal test '{}' returned {}", name, ret);
    }

    println!("[initproc] LTP signal tests done");
}

fn should_enter_debug_shell() -> bool {
    let fd = open("/debug_bash\0", OpenFlags::RDONLY);
    if fd >= 0 {
        let _ = close(fd as usize);
        true
    } else {
        false
    }
}

#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    extern "C" {
        fn _parameter(argc: usize, argv: usize) -> !;
    }
    // initproc is launched directly by kernel and may not have a normal argv stack.
    // Route through user_lib startup with argc=0/argv=0 to initialize heap safely.
    unsafe { _parameter(0, 0) }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let path = "/bash\0";
    let environ = [
        "SHELL=/bash\0".as_ptr(),
        "PWD=/\0".as_ptr(),
        "LOGNAME=root\0".as_ptr(),
        "MOTD_SHOWN=pam\0".as_ptr(),
        "HOME=/root\0".as_ptr(),
        "LANG=C.UTF-8\0".as_ptr(),
        "TERM=vt220\0".as_ptr(),
        "USER=root\0".as_ptr(),
        "SHLVL=0\0".as_ptr(),
        "OLDPWD=/root\0".as_ptr(),
        "PS1=\x1b[1m\x1b[32mNPUCore\x1b[0m:\x1b[1m\x1b[34m\\w\x1b[0m\\$ \0".as_ptr(),
        "_=/bin/bash\0".as_ptr(),
        "PATH=/:/bin\0".as_ptr(),
        "LD_LIBRARY_PATH=/\0".as_ptr(),
        core::ptr::null(),
    ];

    let porgrams = [
        "ls", "cat", "echo", "mkdir", "rmdir", "chown", "chmod", "ln", "basename", "dirname", "rm",
        "grep", "touch", "file", "sleep", "sed", "awk", "head", "tail", "ps", "top", "kill", "cut",
        "free", "df", "du", "mount", "umount", "ping", "netstat", "wget", "curl", "ifconfig", "ip",
        "ss", "nc", "mktemp", "tr",
    ];
    println!(
        "[initproc] preparing busybox \"symlinks\" for programs: {}",
        porgrams.join(", ")
    );
    let program_str = porgrams.join(" ");

    let cmd = format!(
        "/musl/busybox mkdir -p /bin; \
        for c in {} ; do \
           echo '#!/bash' >/bin/$c; \
           echo \"/musl/busybox $c \\\"\\$@\\\"\" >> /bin/$c; \
     done; \
     hash -r",
        program_str
    );
    run_bash_cmd(&cmd, &environ); // prepare busybox "symlinks" for test scripts

    let cfg = load_runtime_config();

    // ============================================================
    // LTP 信号系统测试（控制变量：先验证信号基础，再测网络）
    // ============================================================
    // run_ltp_signal_tests(&environ);

    // ============================================================
    // LTP 网络相关测例（独立 ELF 二进制，跳过 runltp 脚本框架）
    // ============================================================
    // run_ltp_network_tests(&environ);

    // ============================================================
    // Unix Domain Socket 独立测试（不依赖 LTP 框架）
    // 编译自 user/src/bin/unix_test.rs
    // ============================================================
    // run_unix_standalone_tests(&environ);

    // run_bash_cmd("cd musl && ./iperf3 -s &", &environ);
    // run_bash_cmd(
    //     "cd musl && ./netserver -D -L 127.0.0.1 -p 12865 &",
    //     &environ,
    // );
    // sleep(100);
    // run_bash_cmd("cd musl && ./netperf -H 127.0.0.1 -p 12865 -t TCP_CRR -l 1 -- -s 16k -S 16k -m 1k -M 1k -r 64,64 -R 1", &environ);

    // run_bash_cmd("cd musl && bash ./netperf_testcode.sh", &environ);
    // run_bash_cmd("cd musl && bash ./iperf_testcode.sh", &environ); // prepare test scripts (chmod +x etc)

    // run_bash_cmd("cd musl/ltp/testcases/bin && ./send02", &environ);
    // run_bash_cmd("./inet_test", &environ);
    if cfg.mode == RunMode::Shell {
        println!("[initproc] entering shell mode");
        enter_shell(path, &environ);
        shutdown();
        return 0;
    }

    run_selected_groups(&environ, &cfg);

    if cfg.mode == RunMode::RunThenShell {
        println!("[initproc] run_then_shell -> shell");
        enter_shell(path, &environ);
    }

    shutdown();
    0
}
