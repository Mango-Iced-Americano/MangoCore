//! Regression: procfs CPU topology must match the configured SMP machine.

use user_lib::{println, syscall::*};

const O_RDONLY: u32 = 0;

fn read_proc(path: &str, buffer: &mut [u8]) -> Result<usize, isize> {
    let fd = sys_open(path, O_RDONLY);
    if fd < 0 {
        return Err(fd);
    }
    let count = sys_read(fd as usize, buffer);
    let _ = sys_close(fd as usize);
    if count < 0 {
        Err(count)
    } else {
        Ok(count as usize)
    }
}

fn count_prefixed_lines(text: &str, prefix: &str) -> usize {
    text.lines().filter(|line| line.starts_with(prefix)).count()
}

pub fn run() -> i32 {
    println!("[regression_proc_cpu] start");
    let mut cpuinfo_buffer = [0u8; 4096];
    let mut stat_buffer = [0u8; 4096];
    let cpuinfo_len = match read_proc("/proc/cpuinfo\0", &mut cpuinfo_buffer) {
        Ok(len) => len,
        Err(errno) => {
            println!("FAIL: read /proc/cpuinfo returned {}", errno);
            return 1;
        }
    };
    let stat_len = match read_proc("/proc/stat\0", &mut stat_buffer) {
        Ok(len) => len,
        Err(errno) => {
            println!("FAIL: read /proc/stat returned {}", errno);
            return 1;
        }
    };
    let cpuinfo = match core::str::from_utf8(&cpuinfo_buffer[..cpuinfo_len]) {
        Ok(text) => text,
        Err(_) => return 1,
    };
    let stat = match core::str::from_utf8(&stat_buffer[..stat_len]) {
        Ok(text) => text,
        Err(_) => return 1,
    };
    let processors = count_prefixed_lines(cpuinfo, "processor       : ");
    let cpu_rows = stat
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu")
                // 汇总行在 `cpu` 后直接跟空格；per-CPU 行的第一个字符是编号。
                .and_then(|tail| tail.bytes().next())
                .is_some_and(|byte| byte.is_ascii_digit())
        })
        .count();
    println!(
        "  proc cpu detail: processors={} stat_cpu_rows={}",
        processors, cpu_rows
    );
    if processors == 0 || processors != cpu_rows {
        println!("FAIL: procfs CPU topology is missing or inconsistent");
        return 1;
    }
    println!("[regression_proc_cpu] PASS");
    0
}
