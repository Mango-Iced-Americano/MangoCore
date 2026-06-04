#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user_lib::{
    close, exec, exit, getpgid, kill, open, println, read, setpgid, sleep, vfork, waitpid,
    waitpid_wnohang, OpenFlags, SIGKILL, SIGTERM,
};

const DEFAULT_CASE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_CASE_TERM_GRACE_MS: u64 = 1500;
const DEFAULT_LTP_EXCLUDE: &[&str] = &[
    "rt_sigtimedwait01",
    "timerfd04",
    "timerfd_settime02",
];
const DEFAULT_LTP_EXCLUDE_UNSUPPORTED: &[&str] = &[
    "acct01",
    "acct02",
    "acct02_helper",
    "cacheflush01",
    "clock_gettime03",
    "clock_nanosleep03",
    "clone303",
    "futex_waitv01",
    "futex_waitv02",
    "futex_waitv03",
    "get_mempolicy01",
    "get_mempolicy02",
    "memfd_create03",
    "memfd_create04",
    "pkey01",
    "process_madvise01",
    "prctl04",
    "prctl06",
    "prctl06_execve",
    "prctl07",
    "prctl10",
    "set_thread_area01",
    "sgetmask01",
    "ssetmask01",
    "timer_create01",
    "timer_create02",
    "userfaultfd01",
    "ustat01",
    "ustat02",
];
const DEFAULT_LTP_EXCLUDE_MUSL: &[&str] = &[
    "clone04",
    "profil01",
    "sigtimedwait01",
    "sigwaitinfo01",
    "nice04",
];
const DEFAULT_LTP_EXCLUDE_GLIBC: &[&str] = &[];
#[cfg(target_arch = "riscv64")]
const DEFAULT_LTP_EXCLUDE_RV64_MUSL: &[&str] =
    &["epoll_create02", "atof01", "fptest01", "fptest02"];
#[cfg(target_arch = "riscv64")]
const DEFAULT_LTP_EXCLUDE_RV64_GLIBC: &[&str] = &[];
#[cfg(target_arch = "loongarch64")]
const DEFAULT_LTP_EXCLUDE_LA64_MUSL: &[&str] = &["clone08", "clock_gettime04"];
#[cfg(target_arch = "loongarch64")]
const DEFAULT_LTP_EXCLUDE_LA64_GLIBC: &[&str] = &[];

struct LtpCase {
    #[allow(dead_code)]
    index: usize,
    suite: String,
    case_name: String,
    command: String,
}

struct LtpConfig {
    ltp_suites: Vec<String>,
    ltp_from: String,
    ltp_include: Vec<String>,
    ltp_exclude: Vec<String>,
}

struct CliArgs {
    conf_path: String,
    libc: String,
    ltproot: String,
    tmpdir: String,
    #[allow(dead_code)]
    no_group_marker: bool,
    group_timeout_secs: u64,
}

fn parse_cli(argv: &[&str]) -> CliArgs {
    let mut conf_path = String::from("/os_test.conf");
    let mut libc = String::new();
    let mut ltproot = String::new();
    let mut tmpdir = String::from("/tmp");
    let mut no_group_marker = false;
    let mut group_timeout_secs: u64 = 1750;

    let mut i: usize = 1;
    while i < argv.len() {
        match argv[i] {
            "--conf" => {
                i += 1;
                if i < argv.len() {
                    conf_path = String::from(argv[i]);
                }
            }
            "--libc" => {
                i += 1;
                if i < argv.len() {
                    libc = String::from(argv[i]);
                }
            }
            "--ltproot" => {
                i += 1;
                if i < argv.len() {
                    ltproot = String::from(argv[i]);
                }
            }
            "--tmpdir" => {
                i += 1;
                if i < argv.len() {
                    tmpdir = String::from(argv[i]);
                }
            }
            "--no-group-marker" => {
                no_group_marker = true;
            }
            "--group-timeout-secs" => {
                i += 1;
                if i < argv.len() {
                    if let Ok(v) = argv[i].parse::<u64>() {
                        group_timeout_secs = v;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    CliArgs {
        conf_path,
        libc,
        ltproot,
        tmpdir,
        no_group_marker,
        group_timeout_secs,
    }
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
        DEFAULT_LTP_EXCLUDE_RV64_MUSL
    } else if libc == "glibc" {
        DEFAULT_LTP_EXCLUDE_RV64_GLIBC
    } else {
        &[]
    }
}

#[cfg(target_arch = "loongarch64")]
fn default_arch_excludes(libc: &str) -> &[&str] {
    if libc == "musl" {
        DEFAULT_LTP_EXCLUDE_LA64_MUSL
    } else if libc == "glibc" {
        DEFAULT_LTP_EXCLUDE_LA64_GLIBC
    } else {
        &[]
    }
}

fn load_conf(path: &str, libc: &str) -> LtpConfig {
    let mut cfg = LtpConfig {
        ltp_suites: Vec::new(),
        ltp_from: String::new(),
        ltp_include: Vec::new(),
        ltp_exclude: default_excludes(libc),
    };
    let mut conf_exclude = Vec::new();
    let mut conf_libc_exclude = Vec::new();
    let mut conf_arch_libc_exclude = Vec::new();

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
        } else if key == b"ltp_exclude" {
            conf_exclude = comma_split(val_str);
        } else if key == b"ltp_exclude_musl" && libc == "musl" {
            conf_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_glibc" && libc == "glibc" {
            conf_libc_exclude.extend(comma_split(val_str));
        } else if key == b"ltp_exclude_rv64_musl"
            && cfg!(target_arch = "riscv64")
            && libc == "musl"
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

    cfg.ltp_exclude = default_excludes(libc);
    cfg.ltp_exclude.extend(conf_exclude);
    cfg.ltp_exclude.extend(conf_libc_exclude);
    cfg.ltp_exclude.extend(conf_arch_libc_exclude);
    cfg
}

fn parse_runtest_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r').trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let space_pos = trimmed.find(|c: char| c == ' ' || c == '\t')?;
    let case_name = String::from(trimmed[..space_pos].trim());
    let command = String::from(trimmed[space_pos..].trim());
    if case_name.is_empty() || command.is_empty() {
        return None;
    }
    Some((case_name, command))
}

fn parse_suite_file(ltproot: &str, suite: &str, cases: &mut Vec<LtpCase>) -> bool {
    let path = format!("{}/runtest/{}\0", ltproot, suite);
    let fd = open(&path, OpenFlags::RDONLY);
    if fd < 0 {
        println!(
            "[ltprunner] warning: suite '{}' not found at {}",
            suite,
            path.trim_end_matches('\0')
        );
        return false;
    }

    let mut content = Vec::new();
    let mut tmp_buf = [0u8; 1024];
    loop {
        let n = read(fd as usize, &mut tmp_buf);
        if n <= 0 {
            break;
        }
        content.extend_from_slice(&tmp_buf[..n as usize]);
    }
    let _ = close(fd as usize);

    let text = core::str::from_utf8(&content).unwrap_or("");
    let base_idx = cases.len();

    for line in text.lines() {
        if let Some((case_name, command)) = parse_runtest_line(line) {
            cases.push(LtpCase {
                index: base_idx + cases.len() - base_idx,
                suite: String::from(suite),
                case_name,
                command,
            });
        }
    }

    println!(
        "[ltprunner] suite '{}': parsed {} cases",
        suite,
        cases.len() - base_idx
    );
    true
}

fn print_plan(
    cli: &CliArgs,
    cfg: &LtpConfig,
    total_raw: usize,
    from_idx: Option<usize>,
    actual_start_idx: usize,
    filtered: usize,
    from_case: Option<&str>,
) {
    println!("[ltprunner] libc={}", cli.libc);
    println!("[ltprunner] ltproot={}", cli.ltproot);
    println!("[ltprunner] suites={}", cfg.ltp_suites.join(","));
    println!("[ltprunner] raw cases={}", total_raw);

    if !cfg.ltp_include.is_empty() {
        println!("[ltprunner] include={}", cfg.ltp_include.join(","));
    } else {
        println!("[ltprunner] include=none");
    }

    if !cfg.ltp_exclude.is_empty() {
        println!("[ltprunner] exclude={}", cfg.ltp_exclude.join(","));
    } else {
        println!("[ltprunner] exclude=none");
    }

    if !cfg.ltp_from.is_empty() {
        if let Some(idx) = from_idx {
            if let Some(name) = from_case {
                println!(
                    "[ltprunner] ltp_from={} matched case={} index={}",
                    cfg.ltp_from, name, idx
                );
            }
        } else {
            println!(
                "[ltprunner] ltp_from={} not found, starting from beginning",
                cfg.ltp_from
            );
        }
    }

    println!(
        "[ltprunner] filtered={} start_idx={}",
        filtered, actual_start_idx
    );
    println!("[ltprunner] group_timeout_secs={}", cli.group_timeout_secs);
}

fn exit_code_status(raw: i32) -> i32 {
    if raw & 0x7F == 0 {
        (raw >> 8) & 0xFF
    } else {
        128 + (raw & 0x7F)
    }
}

fn reap_orphans() {
    loop {
        let mut status: i32 = 0;
        let ret = waitpid_wnohang(-1, &mut status);
        if ret <= 0 {
            break;
        }
    }
}

fn vfork_with_retry() -> isize {
    for _ in 0..200 {
        let pid = vfork();
        if pid >= 0 {
            return pid;
        }
        // pid < 0: 通常是 EAGAIN(-11)，quota 暂满。
        // 短暂休眠等内核 / initproc 回收刚刚退出的孤儿进程。
        sleep(5);
    }
    // 重试耗尽，返回最后的负值 errno
    vfork()
}

fn cleanup_case_group(case_pgid: isize, own_pgid: isize) {
    if case_pgid <= 0 || case_pgid == own_pgid {
        return;
    }
    // kill 整个进程组，消灭测试留下的孤儿/后台进程
    let pgid_arg = !(case_pgid as usize).wrapping_add(1);
    let _ = kill(pgid_arg, SIGTERM);
    sleep(50);
    let _ = kill(pgid_arg, SIGKILL);
    sleep(10);
    // 非阻塞回收本进程能看到的任何僵尸子进程
    reap_orphans();
}

fn get_time_ms() -> u64 {
    user_lib::get_time() as u64
}

struct PrecomputedEnv {
    ltp_root_s: String,
    path_s: String,
    tmpdir_s: String,
    tmpbase_s: String,
    pwd_s: String,
    env_preload: [*const u8; 16],
    env_no_preload: [*const u8; 16],
}

fn precompute_env(ltproot: &str, tmpdir: &str) -> PrecomputedEnv {
    let ltp_root_s = format!("LTPROOT={}\0", ltproot);
    let path_s = format!(
        "PATH=/bin:/usr/bin:{}/testcases/bin:{}/bin:{}/testcases/lib\0",
        ltproot, ltproot, ltproot
    );
    let tmpdir_s = format!("TMPDIR={}\0", tmpdir);
    let tmpbase_s = format!("TMPBASE={}\0", tmpdir);
    let pwd_s = format!("PWD={}/testcases/bin\0", ltproot);

    let ld_preload_ptr: *const u8 = "LD_PRELOAD=/ltp_proto_compat.so\0".as_ptr();
    let null_ptr: *const u8 = core::ptr::null();

    let env_preload: [*const u8; 16] = [
        ltp_root_s.as_ptr(), path_s.as_ptr(), tmpdir_s.as_ptr(), tmpbase_s.as_ptr(),
        "HOME=/\0".as_ptr(), pwd_s.as_ptr(), "SHELL=/bin/sh\0".as_ptr(),
        "TERM=dumb\0".as_ptr(), "LTP_COLORIZE_OUTPUT=y\0".as_ptr(),
        "LTP_DEV_FS_TYPE=ext2\0".as_ptr(), "LTP_IPC_PATH=/tmp\0".as_ptr(),
        "LANG=C.UTF-8\0".as_ptr(), "LTP_REPRODUCIBLE_OUTPUT=n\0".as_ptr(),
        "KCONFIG_PATH=/proc/config\0".as_ptr(), ld_preload_ptr, null_ptr,
    ];
    let env_no_preload: [*const u8; 16] = [
        ltp_root_s.as_ptr(), path_s.as_ptr(), tmpdir_s.as_ptr(), tmpbase_s.as_ptr(),
        "HOME=/\0".as_ptr(), pwd_s.as_ptr(), "SHELL=/bin/sh\0".as_ptr(),
        "TERM=dumb\0".as_ptr(), "LTP_COLORIZE_OUTPUT=y\0".as_ptr(),
        "LTP_DEV_FS_TYPE=ext2\0".as_ptr(), "LTP_IPC_PATH=/tmp\0".as_ptr(),
        "LANG=C.UTF-8\0".as_ptr(), "LTP_REPRODUCIBLE_OUTPUT=n\0".as_ptr(),
        "KCONFIG_PATH=/proc/config\0".as_ptr(), null_ptr, null_ptr,
    ];

    PrecomputedEnv { ltp_root_s, path_s, tmpdir_s, tmpbase_s, pwd_s, env_preload, env_no_preload }
}

fn run_case(
    case: &LtpCase,
    deadline_ms: u64,
    own_pgid: isize,
    penv: &PrecomputedEnv,
) -> i32 {

    let is_elf = !case.case_name.as_bytes().iter().any(|b| *b == b'.');
    let env: &[*const u8] = if is_elf { &penv.env_preload } else { &penv.env_no_preload };

    let mut cmd_buf = String::from(&case.command);
    cmd_buf.push('\0');

    let pid = vfork_with_retry();
    if pid < 0 {
        return 127;
    }
    if pid == 0 {
        let ret = setpgid(0, 0);
        if ret < 0 {
            exit(126);
        }

        let shell_new = "/bin/bash\0";
        let shell_old = "/bash\0";
        let dash_c = "-c\0";

        let argv: [*const u8; 4] = [
            shell_new.as_ptr(),
            dash_c.as_ptr(),
            cmd_buf.as_ptr(),
            core::ptr::null(),
        ];
        exec(shell_new, &argv, &env);
        let argv2: [*const u8; 4] = [
            shell_old.as_ptr(),
            dash_c.as_ptr(),
            cmd_buf.as_ptr(),
            core::ptr::null(),
        ];
        exec(shell_old, &argv2, &env);
        exit(127);
    }

    let case_pgid = getpgid(pid as usize);
    if case_pgid <= 0 {
        let _ = kill(pid as usize, SIGKILL);
        let mut _code: i32 = 0;
        let _ = waitpid(pid as usize, &mut _code);
        return 137;
    }

    let timeout_ms = DEFAULT_CASE_TIMEOUT_SECS * 1000;
    let mut elapsed_ms: u64 = 0;
    let poll_ms: u64 = 50;
    let mut code: i32 = 0;
    let mut timed_out = false;

    loop {
        let ret = waitpid_wnohang(pid, &mut code);
        if ret == pid {
            break;
        }
        if ret < 0 {
            break;
        }

        elapsed_ms += poll_ms;
        let current = get_time_ms();
        if current > deadline_ms {
            timed_out = true;
            println!(
                "[ltprunner] group deadline reached, killing case {} pgid={}",
                case.case_name, case_pgid
            );
            break;
        }
        if elapsed_ms >= timeout_ms {
            timed_out = true;
            println!(
                "[ltprunner] case {} timeout ({}s), sending SIGTERM to pgid={}",
                case.case_name, DEFAULT_CASE_TIMEOUT_SECS, case_pgid
            );
            break;
        }

        sleep(poll_ms as usize);
    }

    if timed_out {
        let use_pgkill = case_pgid != own_pgid;
        if use_pgkill {
            let _ = kill(!(case_pgid as usize) + 1, SIGTERM);
        } else {
            let _ = kill(pid as usize, SIGTERM);
        }
        let grace_start = get_time_ms();
        loop {
            let ret = waitpid_wnohang(pid, &mut code);
            if ret == pid || ret < 0 {
                break;
            }
            if get_time_ms() - grace_start >= DEFAULT_CASE_TERM_GRACE_MS {
                break;
            }
            sleep(50);
        }

        let ret = waitpid_wnohang(pid, &mut code);
        if ret != pid {
            if use_pgkill {
                let _ = kill(!(case_pgid as usize) + 1, SIGKILL);
            } else {
                let _ = kill(pid as usize, SIGKILL);
            }
            let _ = waitpid(pid as usize, &mut code);
        }
        return 124;
    }

    let ret = exit_code_status(code);
    cleanup_case_group(case_pgid, own_pgid);
    ret
}

#[no_mangle]
fn main(_argc: usize, argv: &[&str]) -> i32 {
    let cli = parse_cli(argv);

    if cli.libc.is_empty() {
        println!("[ltprunner] error: --libc is required");
        return 1;
    }
    if cli.ltproot.is_empty() {
        println!("[ltprunner] error: --ltproot is required");
        return 1;
    }

    let cfg = load_conf(&cli.conf_path, &cli.libc);

    if cfg.ltp_suites.is_empty() {
        println!(
            "[ltprunner] error: ltp_suites is empty in config {}",
            cli.conf_path
        );
        return 2;
    }

    let own_pgid = getpgid(0);
    let penv = precompute_env(&cli.ltproot, &cli.tmpdir);
    let mut raw_cases: Vec<LtpCase> = Vec::new();
    for suite in &cfg.ltp_suites {
        if suite.is_empty() {
            continue;
        }
        parse_suite_file(&cli.ltproot, suite, &mut raw_cases);
    }

    if raw_cases.is_empty() {
        println!("[ltprunner] warning: no cases found in any suite");
        return 0;
    }

    let from_idx: Option<usize> = if !cfg.ltp_from.is_empty() {
        let mut found: Option<usize> = None;
        for (i, case) in raw_cases.iter().enumerate() {
            if case.case_name == cfg.ltp_from {
                if found.is_some() {
                    println!(
                        "[ltprunner] warning: ltp_from={} matches multiple cases, using first at index {}",
                        cfg.ltp_from, found.unwrap()
                    );
                } else {
                    found = Some(i);
                }
            }
        }
        found
    } else {
        Some(0)
    };

    let include_empty = cfg.ltp_include.is_empty();
    let from_idx_val = from_idx.unwrap_or(0);
    let mut from_case_name: Option<&str> = None;
    let mut filtered_indices: Vec<usize> = Vec::new();
    for (i, case) in raw_cases.iter().enumerate() {
        if i < from_idx_val {
            continue;
        }
        if i == from_idx_val {
            from_case_name = Some(&case.case_name);
        }
        if !include_empty && !cfg.ltp_include.iter().any(|e| e == &case.case_name) {
            continue;
        }
        if cfg.ltp_exclude.iter().any(|e| e == &case.case_name) {
            println!(
                "[ltprunner] skip excluded case {} in suite {}",
                case.case_name, case.suite
            );
            continue;
        }
        filtered_indices.push(i);
    }

    let filtered_count = filtered_indices.len();
    let actual_start_idx = filtered_indices.first().copied().unwrap_or(0);

    print_plan(
        &cli,
        &cfg,
        raw_cases.len(),
        from_idx,
        actual_start_idx,
        filtered_count,
        from_case_name,
    );

    if filtered_count == 0 {
        println!("[ltprunner] no cases to run after filtering");
        return 0;
    }

    let start_ms = get_time_ms();
    let deadline_ms = start_ms + cli.group_timeout_secs * 1000;
    let mut executed: usize = 0;
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut skipped_by_timeout: usize = 0;
    let mut current_suite: &str = "";

    for &idx in &filtered_indices {
        let case = &raw_cases[idx];

        let now = get_time_ms();
        if now >= deadline_ms {
            println!(
                "[ltprunner] group deadline exceeded, stopping. executed={} passed={} failed={} remaining={}",
                executed, passed, failed, filtered_count - executed
            );
            skipped_by_timeout = filtered_count - executed;
            break;
        }

        if case.suite != current_suite {
            current_suite = &case.suite;
            println!("=== LTP SUITE {} ===", current_suite);
        }

        println!(
            "[ltprunner] #{} suite={} case={}",
            idx, case.suite, case.case_name
        );

        println!("RUN LTP CASE {}", case.case_name);

        let ret = run_case(case, deadline_ms, own_pgid, &penv);

        if ret == 0 {
            println!("PASS LTP CASE {} : 0", case.case_name);
            passed += 1;
        } else {
            println!("FAIL LTP CASE {} : {}", case.case_name, ret);
            failed += 1;
        }
        executed += 1;

        reap_orphans();
    }

    println!(
        "[ltprunner] done. executed={} passed={} failed={} skipped={} total_ms={} rate={} cases/min",
        executed, passed, failed, skipped_by_timeout,
        get_time_ms() - start_ms,
        (executed as u64 * 60000).checked_div(get_time_ms() - start_ms).unwrap_or(0),
    );

    0
}
