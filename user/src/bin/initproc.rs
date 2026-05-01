#![no_std]
#![no_main]
// use user_lib::{exit, exec, fork, waitpid, shutdown, sleep};
extern crate alloc;

use alloc::format;
use alloc::string::String;
use user_lib::{
    chdir, close, exec, exit, fork, open, println, read, shutdown, sleep, wait, waitpid, OpenFlags,
};

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
        // waitpid(pid as usize, &mut code);
        loop {
            let ret = waitpid(pid as usize, &mut code);
            if ret == pid as isize || ret < 0 {
                break;
            }
            sleep(10);
        }
        return code;
    }
    -1
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

#[derive(Copy, Clone)]
struct RuntimeConfig {
    mode: RunMode,
    mask: u16,
}

impl RuntimeConfig {
    fn default() -> Self {
        // 12-bit mask for testcase groups:
        // bit0..11 => basic, busybox, lua, libctest, iozone,
        //             unixbench, iperf, libcbench, lmbench,
        //             netperf, cyclictest, ltp
        Self {
            mode: RunMode::Run,
            mask: 0x0fff,
        }
    }
}

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
        }
    }
}

fn load_conf_from(path: &str, cfg: &mut RuntimeConfig) -> bool {
    let fd = open(path, OpenFlags::RDONLY);
    if fd < 0 {
        return false;
    }
    let mut buf = [0u8; 1024];
    let mut len = 0usize;
    loop {
        if len >= buf.len() {
            break;
        }
        let n = read(fd as usize, &mut buf[len..]);
        if n <= 0 {
            break;
        }
        len += n as usize;
    }
    let _ = close(fd as usize);
    apply_conf_bytes(&buf[..len], cfg);
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

fn run_group_in_dir(environ: &[*const u8], dir: &str, script: &str) {
    let pid = fork();
    if pid < 0 {
        println!(
            "[initproc] fork failed for {} in {} ret={}",
            script, dir, pid
        );
        return;
    }
    if pid == 0 {
        println!("[initproc] run {} in {}", script, dir);
        let cd_ret = chdir(dir);
        if cd_ret < 0 {
            println!(
                "[initproc] chdir failed dir={} ret={} when running {}",
                dir, cd_ret, script
            );
            exit(126);
        }
        println!("[initproc] entered {}", dir);
        // --- 核心看门狗机制（非常通用、优雅的解法） ---
        let mut cmd = String::new();

        // 2. 运行真正的测试脚本
        cmd.push_str("./");
        cmd.push_str(script);
        cmd.push('\0');
        // cmd.push_str("; wait");
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
    } else {
        let mut exit_code: i32 = 0;
        println!("[initproc] waiting pid={} for {} in {}", pid, script, dir);
        waitpid(pid as usize, &mut exit_code);
        // // --- 优雅的阻塞等待逻辑 ---
        // loop {
        //     // waitpid 会返回退出的进程 PID（或 -1 表示进程已不存在）
        //     let ret = waitpid(pid as usize, &mut exit_code);

        //     // 只有等到真正的目标 bash 进程退出，或者是抛出 ECHILD (-1) 找不到进程时才跳出
        //     if ret == pid as isize || ret == -1 {
        //         break;
        //     }
        //     // 否则（比如返回了被收割的孤儿进程 PID，或者 0/-2 非阻塞状态），继续等待！
        //     sleep(50); // 避免空转
        // }
        // // ---------------------------
        println!(
            "[initproc] done {} in {} exit_code={}",
            script, dir, exit_code
        );
    }
}

fn run_selected_groups(environ: &[*const u8], mask: u16) {
    println!("[initproc] run_selected_groups start mask=0x{:03X}", mask);
    for (idx, (group_name, script)) in TEST_GROUPS.iter().enumerate() {
        if (mask & (1u16 << idx)) == 0 {
            continue;
        }
        println!("[initproc] select bit{} group={}", idx, group_name);
        run_group_in_dir(environ, "/musl\0", script);
        run_group_in_dir(environ, "/glibc\0", script);
    }
    println!("[initproc] run_selected_groups done");
}

fn run_ltp_network_tests(environ: &[*const u8]) {
    // LTP testcases/bin 中与网络/Socket 相关的测例。
    // 只选独立的 ELF 二进制（不含 .sh 脚本，不含需外部网络服务的测例）。
    let net_cases = [
        // ---- Socket 基础 ----
        "accept01",
        "accept02",
        "accept03",
        "accept4_01",
        "bind01",
        "bind02",
        "bind03",
        "bind04",
        "bind05",
        "bind06",
        "connect01",
        "connect02",
        "listen01",
        // ---- 收发数据 ----
        "recv01",
        "recvfrom01",
        "recvmmsg01",
        "recvmsg01",
        "recvmsg02",
        "recvmsg03",
        "send01",
        "send02",
        "sendmmsg01",
        "sendmmsg02",
        "sendmsg01",
        "sendmsg02",
        "sendmsg03",
        "sendto01",
        "sendto02",
        "sendto03",
        // ---- Socket 选项 / 名称 ----
        "getsockname01",
        "getsockopt01",
        "getsockopt02",
        "setsockopt01",
        "setsockopt02",
        "setsockopt03",
        "setsockopt04",
        "setsockopt05",
        // ---- 网络工具 ----
        "add_ipv6addr",
        "check_icmpv4_connectivity",
        "check_icmpv6_connectivity",
    ];

    // let net_cases = ["accept4_01"];
    let testdir = "/musl/ltp/testcases/bin";

    println!(
        "[initproc] LTP network tests begin ({} cases)",
        net_cases.len()
    );

    for &name in &net_cases {
        let cmd = format!(
            "cd {} && echo '=== LTP-NET: {} ===' && ./{}; echo '=== LTP-NET: {} exit=$? ==='",
            testdir, name, name, name
        );
        let ret = run_bash_cmd(&cmd, environ);
        println!("[initproc] LTP network test '{}' returned {}", name, ret);
    }

    println!("[initproc] LTP network tests done");
}

fn run_ltp_signal_tests(environ: &[*const u8]) {
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
        let cmd = format!(
            "cd {} && echo '=== LTP-SIG: {} ===' && ./{}; echo '=== LTP-SIG: {} exit=$? ==='",
            testdir, name, name, name
        );
        let ret = run_bash_cmd(&cmd, environ);
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
        "free", "df", "du", "mount", "umount", "ping", "netstat", "ifconfig", "ip", "ss", "nc",
        "mktemp", "tr",
    ];
    println!(
        "[initproc] preparing busybox \"symlinks\" for programs: {}",
        porgrams.join(", ")
    );
    let program_str = porgrams.join(" ");

    let cmd = format!(
        "busybox mkdir -p /bin; \
        for c in {} ; do \
           echo '#!/bash' >/bin/$c; \
           echo \"busybox $c \\\"\\$@\\\"\" >> /bin/$c; \
     done; \
     hash -r",
        program_str
    );
    // run_bash_cmd(&cmd, &environ); // prepare busybox "symlinks" for test scripts

    let cfg = load_runtime_config();
    // ============================================================
    // LTP 信号系统测试（控制变量：先验证信号基础，再测网络）
    // ============================================================
    // run_ltp_signal_tests(&environ);

    // ============================================================
    // LTP 网络相关测例（独立 ELF 二进制，跳过 runltp 脚本框架）
    // ============================================================
    // run_ltp_network_tests(&environ);

    // run_bash_cmd(
    //     "cd musl && ./netserver -D -L 127.0.0.1 -p 12865 &",
    //     &environ,
    // );
    // sleep(100);
    // run_bash_cmd("cd musl && ./netperf -H 127.0.0.1 -p 12865 -t TCP_CRR -l 1 -- -s 16k -S 16k -m 1k -M 1k -r 64,64 -R 1", &environ);

    // run_bash_cmd("cd musl && bash ./netperf_testcode.sh", &environ);
    run_bash_cmd("cd musl && bash ./iperf_testcode.sh", &environ); // prepare test scripts (chmod +x etc)
                                                                   // /debug_bash remains the highest-priority emergency switch.
    if cfg.mode == RunMode::Shell {
        println!("[initproc] entering shell mode");
        enter_shell(path, &environ);
        shutdown();
        return 0;
    }

    run_selected_groups(&environ, cfg.mask);

    if cfg.mode == RunMode::RunThenShell {
        println!("[initproc] run_then_shell -> shell");
        enter_shell(path, &environ);
    }

    shutdown();
    0
}
