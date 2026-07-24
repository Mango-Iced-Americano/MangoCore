extern crate alloc;
use alloc::string::String;
use user_lib::{exec, exit, fork, get_time, kill, sleep, waitpid, waitpid_wnohang, SIGKILL};

pub fn run_bash_cmd(command: &str, environ: &[*const u8]) -> i32 { run_bash_cmd_timeout(command, environ, 0) }
pub fn run_bash_cmd_timeout(command: &str, environ: &[*const u8], timeout_secs: u64) -> i32 {
    let pid = fork();
    if pid == 0 {
        let command = String::from(command);
        let shell = "/busybox\0";
        exec(shell, &[shell.as_ptr(), "sh\0".as_ptr(), "-c\0".as_ptr(), command.as_ptr(), core::ptr::null()], environ); exit(127);
    }
    if pid < 0 { return -1; }
    let mut status = 0; let start = get_time() as u64;
    loop { let waited = waitpid_wnohang(pid, &mut status); if waited == pid || waited < 0 { return status; }
        if timeout_secs > 0 && (get_time() as u64).saturating_sub(start) >= timeout_secs * 1000 { let _ = kill(pid as usize, SIGKILL); let _ = waitpid(pid as usize, &mut status); return status; }
        sleep(10);
    }
}
pub fn exit_code(status: i32) -> i32 { if status & 0x7f == 0 { (status >> 8) & 0xff } else { 128 + (status & 0x7f) } }
