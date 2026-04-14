#![no_std]
#![no_main]
// use user_lib::{exit, exec, fork, waitpid, shutdown, sleep};
extern crate alloc;

use alloc::string::String;
use alloc::format;
use user_lib::{chdir, close, exec, exit, fork, open, read, shutdown, wait, waitpid,println, OpenFlags};

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
        waitpid(pid as usize, &mut code);
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
            script,
            dir,
            pid
        );
        return;
    }
    if pid == 0 {
        println!("[initproc] run {} in {}", script, dir);
        let cd_ret = chdir(dir);
        if cd_ret < 0 {
            println!(
                "[initproc] chdir failed dir={} ret={} when running {}",
                dir,
                cd_ret,
                script
            );
            exit(126);
        }
        println!("[initproc] entered {}", dir);
        let mut cmd = String::from("./");
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
        println!("[initproc] exec failed for {} in {} via /bash -c", script, dir);
        exit(127);
    } else {
        let mut exit_code: i32 = 0;
        println!("[initproc] waiting pid={} for {} in {}", pid, script, dir);
        waitpid(pid as usize, &mut exit_code);
        println!(
            "[initproc] done {} in {} exit_code={}",
            script,
            dir,
            exit_code
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

    let porgrams = ["ls","cat", "echo", "mkdir", "rmdir", "chown", "chmod", "ln", "basename", "dirname", "sleep",
        // 文本处理
        "sed", "awk", "head", "tail",
        // 系统工具
        "ps", "top","kill", "free", "df", "du", "mount", "umount",
        // 网络工具
        "ping", "netstat", "ifconfig", "ip", "ss",
    ];

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
    run_bash_cmd(&cmd, &environ); // prepare busybox "symlinks" for test scripts

    let cfg = load_runtime_config();

    run_bash_cmd("/musl/iperf3 -s & /musl/iperf3 -c 127.0.0.1 -t 5 ", &environ);

    // /debug_bash remains the highest-priority emergency switch.
    if should_enter_debug_shell() || cfg.mode == RunMode::Shell {
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
