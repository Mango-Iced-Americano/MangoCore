#![no_std]
#![no_main]

#[path = "bench_runner/mod.rs"]
mod bench_runner;

use bench_runner::Subtest;
use user_lib::{exec, exit, fork, waitpid};

const FIXTURE: &str = "/tmp/lmbench-runner-fixture\0";
const LMBENCH_ALL: &str = "./lmbench_all\0";

const SUBTESTS: [Subtest; 18] = [
    Subtest { name: "lat-syscall-null", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "null\0"] },
    Subtest { name: "lat-syscall-read", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "read\0"] },
    Subtest { name: "lat-syscall-write", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "write\0"] },
    Subtest { name: "lat-syscall-stat", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "stat\0", FIXTURE] },
    Subtest { name: "lat-syscall-fstat", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "fstat\0", FIXTURE] },
    Subtest { name: "lat-syscall-open", program: LMBENCH_ALL, args: &["lat_syscall\0", "-P\0", "1\0", "open\0", FIXTURE] },
    Subtest { name: "lat-syscall-select", program: LMBENCH_ALL, args: &["lat_select\0", "-n\0", "100\0", "-P\0", "1\0", "file\0"] },
    Subtest { name: "lat-syscall-sig", program: LMBENCH_ALL, args: &["lat_sig\0", "-P\0", "1\0", "install\0"] },
    Subtest { name: "lat-pipe", program: LMBENCH_ALL, args: &["lat_pipe\0", "-P\0", "1\0"] },
    Subtest { name: "lat-proc-fork", program: LMBENCH_ALL, args: &["lat_proc\0", "-P\0", "1\0", "fork\0"] },
    Subtest { name: "lat-proc-exec", program: LMBENCH_ALL, args: &["lat_proc\0", "-P\0", "1\0", "exec\0"] },
    Subtest { name: "lat-proc-shell", program: LMBENCH_ALL, args: &["lat_proc\0", "-P\0", "1\0", "shell\0"] },
    Subtest { name: "lat-pagefault", program: LMBENCH_ALL, args: &["lat_pagefault\0", "-P\0", "1\0", FIXTURE] },
    Subtest { name: "lat-mmap", program: LMBENCH_ALL, args: &["lat_mmap\0", "-P\0", "1\0", "64m\0", FIXTURE] },
    Subtest { name: "bw-pipe", program: LMBENCH_ALL, args: &["bw_pipe\0", "-P\0", "1\0"] },
    Subtest { name: "bw-file-rd-io-only", program: LMBENCH_ALL, args: &["bw_file_rd\0", "-P\0", "1\0", "64m\0", "io_only\0", FIXTURE] },
    Subtest { name: "bw-file-rd-open2close", program: LMBENCH_ALL, args: &["bw_file_rd\0", "-P\0", "1\0", "64m\0", "open2close\0", FIXTURE] },
    Subtest { name: "bw-mmap-rd", program: LMBENCH_ALL, args: &["bw_mmap_rd\0", "-P\0", "1\0", "64m\0", "mmap_only\0", FIXTURE] },
];

#[no_mangle]
fn main(_argc: usize, argv: &[&str]) -> i32 {
    if !prepare_fixture() {
        return 2;
    }
    bench_runner::run("lmbench-runner", argv, &SUBTESTS)
}

fn prepare_fixture() -> bool {
    let pid = fork();
    if pid == 0 {
        let busybox = "/busybox\0";
        exec(
            busybox,
            &[
                busybox.as_ptr(),
                "dd\0".as_ptr(),
                "if=/dev/zero\0".as_ptr(),
                "of=/tmp/lmbench-runner-fixture\0".as_ptr(),
                "bs=1M\0".as_ptr(),
                "count=64\0".as_ptr(),
                "status=none\0".as_ptr(),
                core::ptr::null(),
            ],
            &[core::ptr::null()],
        );
        exit(127);
    }
    if pid < 0 {
        return false;
    }
    let mut status = 0;
    if waitpid(pid as usize, &mut status) != pid || status != 0 {
        return false;
    }

    let pid = fork();
    if pid == 0 {
        let busybox = "/busybox\0";
        exec(
            busybox,
            &[
                busybox.as_ptr(),
                "cp\0".as_ptr(),
                "/musl/hello\0".as_ptr(),
                "/tmp/hello\0".as_ptr(),
                core::ptr::null(),
            ],
            &[core::ptr::null()],
        );
        exit(127);
    }
    if pid < 0 {
        return false;
    }
    let mut status = 0;
    waitpid(pid as usize, &mut status) == pid && status == 0
}
