#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::String;
use user_lib::syscall::*;
use user_lib::{exit, println};

fn test_mkdir() -> bool {
    let ret = sys_mkdirat(AT_FDCWD, "/tmp\0", 0o777);
    if ret < 0 {
        println!("  FAIL: mkdirat /tmp returned {}", ret);
        return false;
    }
    println!("  PASS: mkdirat /tmp OK");
    true
}

fn test_create_and_write() -> bool {
    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    const O_TRUNC: u32 = 0o1000;

    let fd = sys_open("/tmp/testfile\0", O_CREAT | O_WRONLY | O_TRUNC);
    if fd < 0 {
        println!("  FAIL: open /tmp/testfile returned {}", fd);
        return false;
    }

    let msg = b"hello filesystem!\n";
    let n = sys_write(fd as usize, msg);
    sys_close(fd as usize);

    if n != msg.len() as isize {
        println!("  FAIL: write returned {} (expected {})", n, msg.len());
        return false;
    }
    println!("  PASS: create+write /tmp/testfile OK ({} bytes)", n);
    true
}

fn test_read() -> bool {
    const O_RDONLY: u32 = 0;

    let fd = sys_open("/tmp/testfile\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: open for read returned {}", fd);
        return false;
    }

    let mut buf = [0u8; 64];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);

    if n < 0 {
        println!("  FAIL: read returned {}", n);
        return false;
    }

    let expected = b"hello filesystem!\n";
    if &buf[..n as usize] != expected {
        println!("  FAIL: read data mismatch");
        return false;
    }
    println!("  PASS: read /tmp/testfile OK");
    true
}

fn test_symlink() -> bool {
    let ret = sys_symlinkat("/tmp/testfile\0", AT_FDCWD, "/tmp/testlink\0");
    if ret < 0 {
        println!("  FAIL: symlinkat returned {}", ret);
        return false;
    }
    println!("  PASS: symlinkat /tmp/testlink -> /tmp/testfile OK");
    true
}

fn test_readlink() -> bool {
    let mut buf = [0u8; 256];
    let n = sys_readlinkat(AT_FDCWD, "/tmp/testlink\0", &mut buf);
    if n < 0 {
        println!("  FAIL: readlinkat returned {}", n);
        return false;
    }

    let target = core::str::from_utf8(&buf[..n as usize]).unwrap_or("???");
    if target != "/tmp/testfile" {
        println!("  FAIL: readlink target='{}' (expected '/tmp/testfile')", target);
        return false;
    }
    println!("  PASS: readlinkat /tmp/testlink -> '{}' OK", target);
    true
}

fn test_read_via_symlink() -> bool {
    const O_RDONLY: u32 = 0;

    let fd = sys_open("/tmp/testlink\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: open symlink returned {}", fd);
        return false;
    }

    let mut buf = [0u8; 64];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);

    if n < 0 {
        println!("  FAIL: read via symlink returned {}", n);
        return false;
    }

    let expected = b"hello filesystem!\n";
    if &buf[..n as usize] != expected {
        println!("  FAIL: read via symlink data mismatch");
        return false;
    }
    println!("  PASS: read via symlink OK");
    true
}

fn test_unlink() -> bool {
    let ret = sys_unlinkat(AT_FDCWD, "/tmp/testlink\0", 0);
    if ret < 0 {
        println!("  FAIL: unlinkat testlink returned {}", ret);
        return false;
    }

    let ret2 = sys_unlinkat(AT_FDCWD, "/tmp/testfile\0", 0);
    if ret2 < 0 {
        println!("  FAIL: unlinkat testfile returned {}", ret2);
        return false;
    }
    println!("  PASS: unlinkat OK");
    true
}

fn test_rmdir() -> bool {
    const AT_REMOVEDIR: u32 = 0x200;
    let ret = sys_unlinkat(AT_FDCWD, "/tmp\0", AT_REMOVEDIR);
    if ret < 0 {
        println!("  FAIL: rmdir /tmp returned {}", ret);
        return false;
    }
    println!("  PASS: rmdir /tmp OK");
    true
}

#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    extern "C" {
        fn _parameter(argc: usize, argv: usize) -> !;
    }
    unsafe { _parameter(0, 0) }
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("=== FS Test Suite ===");

    let mut passed = 0;
    let mut failed = 0;

    println!("[1/7] mkdir");
    if test_mkdir() { passed += 1; } else { failed += 1; }

    println!("[2/7] file create + write");
    if test_create_and_write() { passed += 1; } else { failed += 1; }

    println!("[3/7] file read");
    if test_read() { passed += 1; } else { failed += 1; }

    println!("[4/7] symlink");
    if test_symlink() { passed += 1; } else { failed += 1; }

    println!("[5/7] readlink");
    if test_readlink() { passed += 1; } else { failed += 1; }

    println!("[6/7] read via symlink");
    if test_read_via_symlink() { passed += 1; } else { failed += 1; }

    println!("[7/7] unlink + rmdir");
    if test_unlink() && test_rmdir() { passed += 1; } else { failed += 1; }

    println!("=== FS Test: {}/{} passed ===", passed, passed + failed);
    0
}
