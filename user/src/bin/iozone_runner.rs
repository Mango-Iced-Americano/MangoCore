#![no_std]
#![no_main]

#[path = "bench_runner/mod.rs"]
mod bench_runner;

use bench_runner::Subtest;

const IOZONE: &str = "./iozone\0";
const WRITE_READ: [&str; 10] = ["-t\0", "4\0", "-i\0", "0\0", "-i\0", "1\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const RANDOM_READ: [&str; 10] = ["-t\0", "4\0", "-i\0", "0\0", "-i\0", "2\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const BACKWARDS_READ: [&str; 10] = ["-t\0", "4\0", "-i\0", "0\0", "-i\0", "3\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const STRIDE_READ: [&str; 10] = ["-t\0", "4\0", "-i\0", "0\0", "-i\0", "5\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const FWRITE_FREAD: [&str; 10] = ["-t\0", "4\0", "-i\0", "6\0", "-i\0", "7\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const PWRITE_PREAD: [&str; 10] = ["-t\0", "4\0", "-i\0", "9\0", "-i\0", "10\0", "-r\0", "1k\0", "-s\0", "1m\0"];
const PWRITEV_PREADV: [&str; 10] = ["-t\0", "4\0", "-i\0", "11\0", "-i\0", "12\0", "-r\0", "1k\0", "-s\0", "1m\0"];

const SUBTESTS: [Subtest; 7] = [
    Subtest { name: "write-read", program: IOZONE, args: &WRITE_READ },
    Subtest { name: "random-read", program: IOZONE, args: &RANDOM_READ },
    Subtest { name: "read-backwards", program: IOZONE, args: &BACKWARDS_READ },
    Subtest { name: "stride-read", program: IOZONE, args: &STRIDE_READ },
    Subtest { name: "fwrite-fread", program: IOZONE, args: &FWRITE_FREAD },
    Subtest { name: "pwrite-pread", program: IOZONE, args: &PWRITE_PREAD },
    Subtest { name: "pwritev-preadv", program: IOZONE, args: &PWRITEV_PREADV },
];

#[no_mangle]
fn main(_argc: usize, argv: &[&str]) -> i32 {
    bench_runner::run("iozone-runner", argv, &SUBTESTS)
}
