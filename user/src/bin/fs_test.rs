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

fn test_dangling_symlink() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp2\0", 0o777);

    let ret = sys_symlinkat("/nonexistent\0", AT_FDCWD, "/tmp2/dangle\0");
    if ret < 0 {
        println!("  FAIL: create dangling symlink returned {}", ret);
        return false;
    }

    let mut buf = [0u8; 256];
    let n = sys_readlinkat(AT_FDCWD, "/tmp2/dangle\0", &mut buf);
    if n < 0 {
        println!("  FAIL: readlink dangling symlink returned {}", n);
        return false;
    }
    let target = core::str::from_utf8(&buf[..n as usize]).unwrap_or("???");
    if target != "/nonexistent" {
        println!("  FAIL: dangling symlink target='{}'", target);
        return false;
    }

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp2/dangle\0", O_RDONLY);
    if fd >= 0 {
        println!("  FAIL: open dangling symlink should fail, got fd={}", fd);
        sys_close(fd as usize);
        return false;
    }
    println!("  PASS: dangling symlink (target read OK, open correctly fails with {})", fd);

    sys_unlinkat(AT_FDCWD, "/tmp2/dangle\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp2\0", 0x200);
    true
}

fn test_eloop() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp3\0", 0o777);
    sys_symlinkat("loop\0", AT_FDCWD, "/tmp3/loop\0");

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp3/loop\0", O_RDONLY);
    if fd >= 0 {
        println!("  FAIL: ELOOP should fail, got fd={}", fd);
        sys_close(fd as usize);
        return false;
    }
    if fd != -40 {
        println!("  FAIL: ELOOP expected -40 (ELOOP), got {}", fd);
        return false;
    }
    println!("  PASS: ELOOP detection OK (errno={})", -fd);

    sys_unlinkat(AT_FDCWD, "/tmp3/loop\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp3\0", 0x200);
    true
}

fn test_symlink_chain() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp4\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    const O_TRUNC: u32 = 0o1000;
    let fd = sys_open("/tmp4/a\0", O_CREAT | O_WRONLY | O_TRUNC);
    sys_write(fd as usize, b"chain-test\n");
    sys_close(fd as usize);

    sys_symlinkat("a\0", AT_FDCWD, "/tmp4/b\0");
    sys_symlinkat("b\0", AT_FDCWD, "/tmp4/c\0");

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp4/c\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: open chain symlink returned {}", fd);
        return false;
    }
    let mut buf = [0u8; 32];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"chain-test\n" {
        println!("  FAIL: chain symlink read mismatch");
        return false;
    }
    println!("  PASS: symlink chain (a ← b ← c) OK");

    sys_unlinkat(AT_FDCWD, "/tmp4/a\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp4/b\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp4/c\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp4\0", 0x200);
    true
}

fn test_excl_create() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp5\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_EXCL: u32 = 0o200;
    const O_WRONLY: u32 = 0o1;

    let fd = sys_open("/tmp5/exclfile\0", O_CREAT | O_EXCL | O_WRONLY);
    if fd < 0 {
        println!("  FAIL: O_CREAT|O_EXCL create returned {}", fd);
        return false;
    }
    sys_close(fd as usize);

    let fd2 = sys_open("/tmp5/exclfile\0", O_CREAT | O_EXCL | O_WRONLY);
    if fd2 >= 0 {
        println!("  FAIL: O_EXCL on existing file should fail, got fd={}", fd2);
        sys_close(fd2 as usize);
        return false;
    }
    if fd2 != -17 {
        println!("  FAIL: O_EXCL expected -17 (EEXIST), got {}", fd2);
        return false;
    }
    println!("  PASS: O_CREAT|O_EXCL OK (EEXIST={})", -fd2);

    sys_unlinkat(AT_FDCWD, "/tmp5/exclfile\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp5\0", 0x200);
    true
}

fn test_readlink_on_regular() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp6\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp6/regular\0", O_CREAT | O_WRONLY);
    sys_close(fd as usize);

    let mut buf = [0u8; 32];
    let n = sys_readlinkat(AT_FDCWD, "/tmp6/regular\0", &mut buf);
    if n >= 0 {
        println!("  FAIL: readlink on regular file should fail, got {}", n);
        return false;
    }
    if n != -22 {
        println!("  FAIL: expected -22 (EINVAL), got {}", n);
        return false;
    }
    println!("  PASS: readlink on regular file returns EINVAL ({})", -n);

    sys_unlinkat(AT_FDCWD, "/tmp6/regular\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp6\0", 0x200);
    true
}

fn test_unlink_symlink_preserves_target() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp7\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp7/target\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"preserved\n");
    sys_close(fd as usize);

    sys_symlinkat("/tmp7/target\0", AT_FDCWD, "/tmp7/link\0");

    sys_unlinkat(AT_FDCWD, "/tmp7/link\0", 0);

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp7/target\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: target file gone after symlink unlink, err={}", fd);
        return false;
    }
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"preserved\n" {
        println!("  FAIL: target content changed after symlink unlink");
        return false;
    }
    println!("  PASS: unlink symlink preserves target OK");

    sys_unlinkat(AT_FDCWD, "/tmp7/target\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp7\0", 0x200);
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

    println!("[1/13] mkdir");
    if test_mkdir() { passed += 1; } else { failed += 1; }

    println!("[2/13] file create + write");
    if test_create_and_write() { passed += 1; } else { failed += 1; }

    println!("[3/13] file read");
    if test_read() { passed += 1; } else { failed += 1; }

    println!("[4/13] symlink");
    if test_symlink() { passed += 1; } else { failed += 1; }

    println!("[5/13] readlink");
    if test_readlink() { passed += 1; } else { failed += 1; }

    println!("[6/13] read via symlink");
    if test_read_via_symlink() { passed += 1; } else { failed += 1; }

    println!("[7/13] unlink + rmdir");
    if test_unlink() && test_rmdir() { passed += 1; } else { failed += 1; }

    println!("[8/13] dangling symlink");
    if test_dangling_symlink() { passed += 1; } else { failed += 1; }

    println!("[9/13] ELOOP detection");
    if test_eloop() { passed += 1; } else { failed += 1; }

    println!("[10/13] symlink chain");
    if test_symlink_chain() { passed += 1; } else { failed += 1; }

    println!("[11/13] O_CREAT|O_EXCL");
    if test_excl_create() { passed += 1; } else { failed += 1; }

    println!("[12/13] readlink on regular file");
    if test_readlink_on_regular() { passed += 1; } else { failed += 1; }

    println!("[13/13] unlink symlink preserves target");
    if test_unlink_symlink_preserves_target() { passed += 1; } else { failed += 1; }

    println!("=== FS Test: {}/{} passed ===", passed, passed + failed);
    0
}
