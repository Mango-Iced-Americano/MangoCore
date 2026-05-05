#![no_std]
#![no_main]
// use user_lib::{exit, exec, fork, waitpid, shutdown, sleep};
extern crate alloc;

use core::net;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
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

fn is_iperf_script(script: &str) -> bool {
    script.contains("iperf")
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
        if is_iperf_script(script) {
            // iperf 测试以守护进程方式运行，waitpid 无法等到其结束。
            // 直接计时 15 秒（musl 和 glibc 都是 15 秒）后继续。
            println!(
                "[initproc] iperf detected, using timer (15s) for {} in {}",
                script, dir
            );
            sleep(15000);
            // 尝试收割子进程，不阻塞等待
            let _ = waitpid(pid as usize, &mut exit_code);
        } else {
            println!("[initproc] waiting pid={} for {} in {}", pid, script, dir);
            waitpid(pid as usize, &mut exit_code);
        }
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

fn run_unix_standalone_tests(environ: &[*const u8]) {
    // 独立的 Unix Domain Socket 测试程序（完全不依赖 LTP 框架）
    // 编译自 user/src/bin/unix_test.rs
    let testdir = "/";
    let name = "unix_test";
    let cmd = format!(
        "cd {} && echo '=== STANDALONE UNIX TEST: {} ===' && ./{}; echo '=== STANDALONE UNIX TEST: {} exit=$? ==='",
        testdir, name, name, name
    );
    let ret = run_bash_cmd(&cmd, environ);
    println!(
        "[initproc] standalone unix test '{}' returned {}",
        name, ret
    );
}

fn run_ltp_network_tests(environ: &[*const u8]) {
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
        "ppoll01",
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
    // let net_cases: Vec<&str> = socket_syscall_cases
    //     .iter()
    //     .chain(data_io_cases.iter())
    //     .chain(socket_opt_cases.iter())
    //     .chain(net_tool_cases.iter())
    //     .chain(net_adv_cases.iter())
    //     .chain(io_multiplex_cases.iter())
    //     .chain(ipv6_cases.iter())
    //     // .chain(unix_socket_cases.iter())
    //     //    .chain(net_shell_cases.iter())
    //     .copied()
    //     .collect();

    // let net_cases: Vec<&str> = vec!["send02"];

    let net_cases: Vec<&str> = unix_socket_cases.iter().copied().collect();
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
    run_ltp_network_tests(&environ);

    // ============================================================
    // Unix Domain Socket 独立测试（不依赖 LTP 框架）
    // 编译自 user/src/bin/unix_test.rs
    // ============================================================
    // run_unix_standalone_tests(&environ);

    // run_bash_cmd(
    //     "cd musl && ./netserver -D -L 127.0.0.1 -p 12865 &",
    //     &environ,
    // );
    // sleep(100);
    // run_bash_cmd("cd musl && ./netperf -H 127.0.0.1 -p 12865 -t TCP_CRR -l 1 -- -s 16k -S 16k -m 1k -M 1k -r 64,64 -R 1", &environ);

    // run_bash_cmd("cd musl && bash ./netperf_testcode.sh", &environ);
    // run_bash_cmd("cd musl && bash ./iperf_testcode.sh", &environ); // prepare test scripts (chmod +x etc)

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
