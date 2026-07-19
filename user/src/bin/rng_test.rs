#![no_std]
#![no_main]

use user_lib::println;
use user_lib::syscall::{sys_close, sys_getrandom, sys_open, sys_read};

const EINVAL: isize = -22;
const GRND_RANDOM: u32 = 0x0002;
const GRND_INSECURE: u32 = 0x0004;

fn looks_live(bytes: &[u8]) -> bool {
    bytes.iter().any(|byte| *byte != 0) && bytes.windows(2).any(|pair| pair[0] != pair[1])
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    let mut first = [0u8; 64];
    let mut second = [0u8; 64];
    if sys_getrandom(&mut first, 0) != first.len() as isize
        || sys_getrandom(&mut second, 0) != second.len() as isize
    {
        println!("[rng-test] FAIL: secure getrandom unavailable");
        return 1;
    }
    if !looks_live(&first) || !looks_live(&second) || first == second {
        println!("[rng-test] FAIL: repeated or stuck getrandom output");
        return 1;
    }

    let mut byte = [0u8; 1];
    if sys_getrandom(&mut byte, 0x8000_0000) != EINVAL
        || sys_getrandom(&mut byte, GRND_RANDOM | GRND_INSECURE) != EINVAL
    {
        println!("[rng-test] FAIL: getrandom flag validation");
        return 1;
    }

    let fd = sys_open("/dev/urandom\0", 0);
    let mut device_bytes = [0u8; 64];
    if fd < 0
        || sys_read(fd as usize, &mut device_bytes) != device_bytes.len() as isize
        || !looks_live(&device_bytes)
        || device_bytes == first
        || sys_close(fd as usize) != 0
    {
        println!("[rng-test] FAIL: /dev/urandom");
        return 1;
    }

    println!("[rng-test] PASS: getrandom and /dev/urandom are live and distinct");
    0
}
