//! Regression: mmap edge cases (len=0, MAP_FIXED, mprotect)
//! Bug: mmap len=0 could crash kernel or corrupt VMA state.
//!      mprotect on ranges with a page-aligned start and partial-page length
//!      had boundary bugs.
//! Expected: len=0 returns -EINVAL. mprotect rounds the length up to a page.
//!           No kernel panic or memory corruption.
//! Related subsystem: mm / VMA
//! LTP counterpart: mmap02, mmap03, mprotect01
//! Source: Oracle audit — mmap edge cases

use user_lib::syscall::*;
use user_lib::println;

const EINVAL: isize = -22;
const PROT_READ: usize = 1;
const PROT_READ_WRITE: usize = 3;
const MAP_PRIVATE: usize = 2;
const MAP_ANONYMOUS: usize = 0x20;
const MAP_FIXED: usize = 0x10;

pub fn run() -> i32 {
    println!("[regression_mmap_edge_cases] start");

    // ── Test 1: mmap with len=0 → should return -EINVAL ──────────────
    {
        let ret = sys_mmap(0, 0, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0);
        println!("  mmap(len=0) returned {} (expect {})", ret, EINVAL);
        if ret != EINVAL {
            println!("FAIL: mmap(len=0) should return -EINVAL");
            return 1;
        }
    }

    // ── Test 2: mmap two pages, then mprotect a partial-page length ───
    {
        let page = sys_mmap(0, 8192, PROT_READ_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0);
        if page < 0 {
            println!("FAIL: mmap for mprotect test returned {}", page);
            return 1;
        }
        let addr = page as usize;
        println!("  allocated page at 0x{:x}", addr);

        // Write a value to the page (prove it's writable)
        unsafe { (addr as *mut u8).write_volatile(0x42); }

        // Linux requires a page-aligned start address, while a non-page-sized
        // length is rounded up. Protect the first half-length of the second
        // page; the whole second page becomes read-only.
        let ret = sys_mprotect(addr + 4096, 2048, PROT_READ);
        println!("  mprotect(0x{:x}, 2048, PROT_READ) returned {}", addr + 4096, ret);
        if ret != 0 {
            println!("FAIL: mprotect with partial-page length failed");
            let _ = sys_munmap(addr, 8192);
            return 1;
        }

        // The separate first page must remain writable.
        unsafe { (addr as *mut u8).write_volatile(0x43); }
        println!("  first page write OK");

        // Verify first-page readback.
        let v = unsafe { (addr as *const u8).read_volatile() };
        if v != 0x43 {
            println!("FAIL: unexpected value 0x{:x} at addr", v);
            let _ = sys_munmap(addr, 8192);
            return 1;
        }
        println!("  readback OK (0x{:x})", v);

        let _ = sys_munmap(addr, 8192);
    }

    // ── Test 3: mmap MAP_FIXED over existing mapping ─────────────────
    {
        // Allocate two pages
        let p1 = sys_mmap(0, 8192, PROT_READ_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, 0, 0);
        if p1 < 0 {
            println!("FAIL: initial mmap returned {}", p1);
            return 1;
        }
        let addr = p1 as usize;

        // Write marker
        unsafe { (addr as *mut u8).write_volatile(0xAA); }

        // Try MAP_FIXED at addr+4096 (second page) — Linux allows this,
        // it replaces the existing mapping. We just check no crash.
        let p2 = sys_mmap(addr + 4096, 4096, PROT_READ,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, 0, 0);
        println!("  MAP_FIXED at 0x{:x} returned 0x{:x}", addr + 4096, p2);
        // MAP_FIXED should succeed or return an appropriate error,
        // but must NOT crash the kernel.
        if p2 < 0 {
            println!("  MAP_FIXED returned {} (acceptable error)", p2);
        } else {
            // Read back old marker from first page (should survive)
            let v = unsafe { (addr as *const u8).read_volatile() };
            if v != 0xAA {
                println!("FAIL: first page corrupted by MAP_FIXED");
                let _ = sys_munmap(addr, 8192);
                return 1;
            }
            println!("  first page marker intact after MAP_FIXED");
        }

        let _ = sys_munmap(addr, 8192);
    }

    println!("[regression_mmap_edge_cases] PASS");
    0
}
