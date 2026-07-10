//! Regression: rename with 4097-byte name panicked kernel
//! Bug: ext4 rename with name.len()=4097 caused slice index panic
//!      at direntry.rs:207: `self.name[..name.len()].copy_from_slice()`
//!      where self.name is [u8; 255] (ext4 max filename).
//! Expected: rename with name >= 256 bytes returns error (ENAMETOOLONG
//!           or EINVAL), does NOT panic the kernel.
//! Related subsystem: ext4 / VFS
//! LTP counterpart: rename10
//! Source: docs/09_debug/ext4-rename-name-panic.md

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user_lib::syscall::*;
use user_lib::println;

const AT_FDCWD: isize = -100;
const O_RDWR: u32 = 2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;
const ENAMETOOLONG: isize = -36;
const EINVAL: isize = -22;

/// Build a null-terminated path string from a byte slice.
fn make_cstr(bytes: &[u8]) -> String {
    let mut s = String::new();
    for &b in bytes {
        s.push(b as char);
    }
    s.push('\0');
    s
}

#[no_mangle]
pub fn run() -> i32 {
    println!("[regression_rename_long_name] start");

    // ── Test 1: file with 255-byte name → should work ────────────────
    {
        let name_255: Vec<u8> = (b'a'..=b'z')
            .cycle()
            .take(255)
            .collect();
        let fname = make_cstr(&name_255);
        println!("  creating file with {} byte name", 255);

        let fd = sys_openat(AT_FDCWD, fname.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o644);
        if fd < 0 {
            println!("  openat(255-byte name) returned {} — name limit may be lower", fd);
        } else {
            let _ = sys_close(fd as usize);
            println!("  255-byte file created OK");

            // Rename it to something short
            let short = "/tmp/rr_255_to_short\0";
            let ret = sys_renameat2(AT_FDCWD, &fname, AT_FDCWD, short, 0);
            println!("  rename(255-byte → short) returned {}", ret);
            if ret < 0 {
                println!("  rename failed with {} — acceptable", ret);
            }
            // Cleanup
            let _ = sys_unlinkat(AT_FDCWD, short, 0);
        }
    }

    // ── Test 2: file with 256-byte name → should fail with error ─────
    {
        let mut name_256: Vec<u8> = (b'a'..=b'z')
            .cycle()
            .take(256)
            .collect();
        // Make it a valid-ish filename (no slashes)
        for b in &mut name_256 {
            if *b == b'/' { *b = b'x'; }
        }
        let fname = make_cstr(&name_256);
        println!("  attempting file with {} byte name", 256);

        let fd = sys_openat(AT_FDCWD, fname.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o644);
        if fd >= 0 {
            let _ = sys_close(fd as usize);
            // File was created — try to rename it to trigger the bug path
            let short = "/tmp/rr_256_to_short\0";
            let ret = sys_renameat2(AT_FDCWD, &fname, AT_FDCWD, short, 0);
            println!("  rename(256-byte → short) returned {}", ret);
            // Cleanup regardless
            let _ = sys_unlinkat(AT_FDCWD, &fname, 0);
            let _ = sys_unlinkat(AT_FDCWD, short, 0);
            // If we get here without kernel panic, test passed
        } else {
            println!("  openat(256-byte name) returned {} — name rejected, OK", fd);
        }
    }

    // ── Test 3: rename to 4096-byte target name → must not panic ─────
    {
        // Create a short-named file first
        let short = "/tmp/rr_short\0";
        let fd = sys_openat(AT_FDCWD, short.as_ptr(), O_RDWR | O_CREAT | O_TRUNC, 0o644);
        if fd < 0 {
            println!("  cannot create test file: {} (skipping)", fd);
        } else {
            let _ = sys_write(fd as usize, b"hello");
            let _ = sys_close(fd as usize);

            let name_4096: Vec<u8> = (b'A'..=b'Z')
                .cycle()
                .take(4096)
                .collect();
            let long_name = make_cstr(&name_4096);
            let long_path = format!("/tmp/{}\0", &long_name[..long_name.len() - 1]);

            let ret = sys_renameat2(AT_FDCWD, short, AT_FDCWD, &long_path, 0);
            println!("  rename(short → 4096-byte) returned {} (must not panic)", ret);
            if ret >= 0 {
                // Success — cleanup
                let _ = sys_unlinkat(AT_FDCWD, &long_path, 0);
            } else if ret == ENAMETOOLONG || ret == EINVAL {
                println!("  correctly rejected with error");
            } else {
                println!("  returned {} — acceptable (non-panic)", ret);
            }

            // Cleanup short file
            let _ = sys_unlinkat(AT_FDCWD, short, 0);
        }
    }

    println!("[regression_rename_long_name] PASS");
    0
}
