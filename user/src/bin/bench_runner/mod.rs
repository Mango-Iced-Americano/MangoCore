extern crate alloc;

use alloc::format;
use user_lib::{chdir, close, exec, exit, fork, open, println, read, waitpid, write, OpenFlags};

const STATS_FILES: [&str; 24] = [
    "boot", "taskq", "timer", "seccomp", "syscall", "ctxsw", "reclaim", "tlb", "heap",
    "anon_unmap", "pagecache", "blockio", "net", "ext4", "resource", "buddyinfo", "zombies",
    "pipe", "lwext4", "mount", "syscall_top", "pagefault", "vm", "features",
];

pub struct Subtest {
    pub name: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
}

pub fn run(label: &str, argv: &[&str], subtests: &[Subtest]) -> i32 {
    let workdir = argv.get(1).copied().unwrap_or(".");
    if chdir(workdir) < 0 {
        println!("[{}] error: cannot chdir to {}", label, workdir);
        return 2;
    }
    println!("[{}] workdir={}", label, workdir);

    let mut failed = 0;
    for subtest in subtests {
        println!("[{}] === subtest {} ===", label, subtest.name);
        let stats_available = write_control("/sys/kernel/stats/stats_on\0", b"0\n");
        if stats_available {
            let _ = write_control("/sys/kernel/stats/profile\0", b"memory_io\n");
            let _ = write_control("/sys/kernel/stats/reset\0", b"1\n");
            snapshot(label, "stats-before");
        }

        let stats_enabled = stats_available && write_control("/sys/kernel/stats/stats_on\0", b"1\n");
        let status = run_subtest(subtest);
        if stats_available {
            let _ = write_control("/sys/kernel/stats/stats_on\0", b"0\n");
            snapshot(label, "stats-after");
        }
        println!(
            "[{}] subtest={} exit_code={} stats_enabled={}",
            label, subtest.name, status, stats_enabled
        );
        println!("[{}] === end ===", label);
        if status != 0 {
            failed += 1;
        }
    }

    println!("[{}] completed subtests={} failed={}", label, subtests.len(), failed);
    if failed == 0 { 0 } else { 1 }
}

fn run_subtest(subtest: &Subtest) -> i32 {
    let pid = fork();
    if pid == 0 {
        let mut command = format!("exec {}", subtest.program.trim_end_matches('\0'));
        for arg in subtest.args {
            command.push(' ');
            command.push_str(arg.trim_end_matches('\0'));
        }
        command.push('\0');
        let shell = "/bin/sh\0";
        let dash_c = "-c\0";
        exec(
            shell,
            &[shell.as_ptr(), dash_c.as_ptr(), command.as_ptr(), core::ptr::null()],
            &[core::ptr::null()],
        );
        exit(127);
    }
    if pid < 0 {
        return 127;
    }
    let mut status = 0;
    if waitpid(pid as usize, &mut status) < 0 {
        return 127;
    }
    decode_exit_status(status)
}

fn write_control(path: &str, contents: &[u8]) -> bool {
    let fd = open(path, OpenFlags::WRONLY);
    if fd < 0 {
        return false;
    }
    let result = write(fd as usize, contents) == contents.len() as isize;
    let _ = close(fd as usize);
    result
}

fn snapshot(label: &str, phase: &str) {
    println!("[{}] --- {} ---", label, phase);
    for name in STATS_FILES {
        let path = format!("/sys/kernel/stats/{}\0", name);
        let fd = open(&path, OpenFlags::RDONLY);
        if fd < 0 {
            println!("[{}] stats file={} unavailable", label, name);
            continue;
        }
        println!("[{}] stats file={} begin", label, name);
        let mut buffer = [0; 1024];
        let mut ended_with_newline = true;
        loop {
            let count = read(fd as usize, &mut buffer);
            if count <= 0 {
                break;
            }
            let Ok(count) = usize::try_from(count) else {
                break;
            };
            let _ = write(1, &buffer[..count]);
            ended_with_newline = buffer[count - 1] == b'\n';
        }
        if !ended_with_newline {
            println!("");
        }
        let _ = close(fd as usize);
        println!("[{}] stats file={} end", label, name);
    }
    println!("[{}] --- {} end ---", label, phase);
}

fn decode_exit_status(status: i32) -> i32 {
    if status & 0x7f == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    }
}
