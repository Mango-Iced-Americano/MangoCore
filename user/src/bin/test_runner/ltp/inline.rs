extern crate alloc;
use alloc::format;
use alloc::string::String;
use user_lib::{chdir, close, exit, fork, getdents64, open, println, waitpid, OpenFlags};
use crate::runner::ltp::policy::{exact, prefixes};
use crate::runner::process::{exit_code, run_bash_cmd_timeout};
pub fn run_ltp_binaries(environ: &[*const u8], dir: &str, exclude: &[String], include: &[String], from: Option<&str>, timeout: u64) {
    let root = dir.trim_end_matches('\0'); let suffix = if root.contains("musl") { "musl" } else { "glibc" };
    println!("#### OS COMP TEST GROUP START ltp-{} ####", suffix);
    if fork() == 0 {
        if chdir(dir) < 0 { exit(126); } let fd = open("ltp/testcases/bin\0", OpenFlags::RDONLY); if fd < 0 { exit(0); }
        let mut names = [0u8; 16384]; let mut used = 0usize; let mut buffer = [0u8; 4096];
        loop { let count = getdents64(fd as usize, &mut buffer); if count <= 0 { break; } let mut offset = 0; while offset + 19 <= count as usize { let length = u16::from_ne_bytes([buffer[offset + 16], buffer[offset + 17]]) as usize; if length < 19 || offset + length > count as usize { break; } let start = offset + 19; let end = buffer[start..offset + length].iter().position(|v| *v == 0).map(|v| start + v).unwrap_or(offset + length); if end > start && used + end - start + 1 <= names.len() { names[used..used + end - start].copy_from_slice(&buffer[start..end]); used += end - start; names[used] = 0; used += 1; } offset += length; } }
        let _ = close(fd as usize); let mut offset = 0; let mut started = from.is_none(); while offset < used { let end = names[offset..used].iter().position(|v| *v == 0).map(|v| offset + v).unwrap_or(used); let name = core::str::from_utf8(&names[offset..end]).unwrap_or(""); offset = end + 1; if !started { started = Some(name) == from; if !started { println!("SKIP LTP CASE {} : before ltp_from", name); continue; } } if !include.is_empty() && !include.iter().any(|value| value == name) { continue; } if exclude.iter().any(|value| value == name) { println!("SKIP LTP CASE {} : excluded", name); continue; } if include.is_empty() { if let Some(reason) = prefixes::skip(name).or_else(|| exact::skip(name)) { println!("SKIP LTP CASE {} : {}", name, reason); continue; } } let command = format!("export LTPROOT={}/ltp; export LTP_IPC_PATH=/tmp; export PATH={}/ltp/testcases/bin:$PATH; ./ltp/testcases/bin/{}\0", root, root, name); let code = exit_code(run_bash_cmd_timeout(&command, environ, 30)); println!("{} LTP CASE {} : {}", if code == 0 { "DONE" } else { "FAIL" }, name, code); }
        println!("#### OS COMP TEST GROUP END ltp-{} ####", suffix); exit(0);
    }
    let mut status = 0; let _ = waitpid(!0, &mut status); let _ = timeout; println!("#### OS COMP TEST GROUP END ltp-{} ####", suffix);
}
