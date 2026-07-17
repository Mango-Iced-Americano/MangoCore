use alloc::string::String;
use user_lib::{
    exec, exit, getpgid, kill, println, setpgid, sleep, vfork, waitpid, waitpid_wnohang, SIGKILL,
    SIGTERM,
};

use super::environment::PrecomputedEnv;
use super::suite::LtpCase;

const DEFAULT_CASE_TERM_GRACE_MS: u64 = 1500;

pub fn get_time_ms() -> u64 {
    user_lib::get_time() as u64
}

pub fn reap_orphans() {
    loop {
        let mut status: i32 = 0;
        let ret = waitpid_wnohang(-1, &mut status);
        if ret <= 0 {
            break;
        }
    }
}

fn exit_code_status(raw: i32) -> i32 {
    if raw & 0x7F == 0 {
        (raw >> 8) & 0xFF
    } else {
        128 + (raw & 0x7F)
    }
}

fn vfork_with_retry() -> isize {
    for _ in 0..200 {
        let pid = vfork();
        if pid >= 0 {
            return pid;
        }
        sleep(5);
    }
    vfork()
}

fn cleanup_case_group(case_pgid: isize, own_pgid: isize) {
    if case_pgid <= 0 || case_pgid == own_pgid {
        return;
    }
    let pgid_arg = !(case_pgid as usize).wrapping_add(1);
    let _ = kill(pgid_arg, SIGTERM);
    sleep(50);
    let _ = kill(pgid_arg, SIGKILL);
    sleep(10);
    reap_orphans();
}

pub fn run_case(
    case: &LtpCase,
    deadline_ms: u64,
    own_pgid: isize,
    penv: &PrecomputedEnv,
    case_timeout_secs: u64,
) -> i32 {
    let is_elf = !case.case_name.as_bytes().iter().any(|b| *b == b'.');
    let env: &[*const u8] = if is_elf {
        &penv.env_preload
    } else {
        &penv.env_no_preload
    };
    let mut cmd_buf = String::from(&case.command);
    cmd_buf.push('\0');

    let pid = vfork_with_retry();
    if pid < 0 {
        return 127;
    }
    if pid == 0 {
        if setpgid(0, 0) < 0 {
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
        exec(shell_new, &argv, env);
        let argv2: [*const u8; 4] = [
            shell_old.as_ptr(),
            dash_c.as_ptr(),
            cmd_buf.as_ptr(),
            core::ptr::null(),
        ];
        exec(shell_old, &argv2, env);
        exit(127);
    }

    let case_pgid = getpgid(pid as usize);
    if case_pgid <= 0 {
        let _ = kill(pid as usize, SIGKILL);
        let mut code: i32 = 0;
        let _ = waitpid(pid as usize, &mut code);
        return 137;
    }

    let timeout_ms = case_timeout_secs * 1000;
    let mut elapsed_ms: u64 = 0;
    let poll_ms: u64 = 50;
    let mut code: i32 = 0;
    let mut timed_out = false;
    loop {
        let ret = waitpid_wnohang(pid, &mut code);
        if ret == pid || ret < 0 {
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
                case.case_name, case_timeout_secs, case_pgid
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
            if ret == pid || ret < 0 || get_time_ms() - grace_start >= DEFAULT_CASE_TERM_GRACE_MS {
                break;
            }
            sleep(50);
        }
        if waitpid_wnohang(pid, &mut code) != pid {
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
