//! Regression: lwext4 cold reopen after truncate must zero-fill sparse tail
//! Bug: kernel had a bad pre-shrink zero-write that corrupted sparse file
//!      semantics. After the kernel patch removed it, sparse-tail reads are
//!      zero-filled by LwExt4PageCacheBackend. This regression exercises both
//!      single-page read_page and batch read_pages for sparse holes beyond
//!      physical EOF, plus post-write marker persistence across close→reopen.
//! Expected: After write→fsync→close→open→shrink→fsync→close→open→extend,
//!           page 0 retains its written pattern; single-page read of hole
//!           page 1 returns zeros; 8192-byte batch read spanning pages 1-2
//!           returns zeros; marker written to page 3 persists after
//!           fsync→close→reopen while adjacent hole pages 2,4 stay zero.
//! Related subsystem: lwext4 / PageCache / read_page + read_pages
//! Source: kernel pre-shrink zero-write patch fix regression

#![no_std]
#![no_main]

use user_lib::syscall::*;
use user_lib::println;

const PAGE_SIZE: usize = 4096;
const TWO_PAGES: usize = 8192;
const N_PAGES: usize = 8;
const FILE_SIZE: usize = N_PAGES * PAGE_SIZE;

const O_RDWR: u32 = 0o2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;

/// Unique test file on /sdcard (NUL-terminated, pure ASCII).
const TEST_PATH: &str = "/sdcard/reg_lwext4_trunc_hole\0";

/// Single-page read+verify helper: seeks to `offset`, reads `PAGE_SIZE` bytes,
/// checks all bytes equal `expect_val`. Returns 0 on success, reports
/// mismatches and returns 1 on failure (caller must propagate).
fn verify_page(fd: usize, offset: isize, expect_val: u8, label: &str) -> i32 {
    let mut buf = [0u8; PAGE_SIZE];
    let pos = sys_lseek(fd, offset, SEEK_SET);
    if pos != offset {
        println!("FAIL: {} lseek returned {} (expected {})", label, pos, offset);
        return 1;
    }
    let n = sys_read(fd, &mut buf);
    if n != PAGE_SIZE as isize {
        println!("FAIL: {} read returned {} (expected {})", label, n, PAGE_SIZE);
        return 1;
    }
    for (i, &b) in buf.iter().enumerate() {
        if b != expect_val {
            println!("FAIL: {} byte {} is 0x{:02x} (expected 0x{:02x})", label, i, b, expect_val);
            return 1;
        }
    }
    println!("  {}: {} bytes all 0x{:02x} OK", label, PAGE_SIZE, expect_val);
    0
}

/// Batch read+verify helper: seeks to `offset`, reads `len` bytes, checks all
/// bytes equal `expect_val`. Accepts any len, not just PAGE_SIZE multiples.
fn verify_batch(fd: usize, offset: isize, len: usize, expect_val: u8, label: &str) -> i32 {
    // Fixed-size stack buffer; caller must not exceed TWO_PAGES.
    let mut buf = [0u8; TWO_PAGES];
    let pos = sys_lseek(fd, offset, SEEK_SET);
    if pos != offset {
        println!("FAIL: {} lseek returned {} (expected {})", label, pos, offset);
        return 1;
    }
    let n = sys_read(fd, &mut buf[..len]);
    if n != len as isize {
        println!("FAIL: {} read returned {} (expected {})", label, n, len);
        return 1;
    }
    for (i, &b) in buf[..len].iter().enumerate() {
        if b != expect_val {
            println!("FAIL: {} byte {} is 0x{:02x} (expected 0x{:02x})", label, i, b, expect_val);
            return 1;
        }
    }
    println!("  {}: {} bytes all 0x{:02x} OK", label, len, expect_val);
    0
}

pub fn run() -> i32 {
    println!("[regression_lwext4_trunc_hole] start: {} pages, cold-reopen sparse-hole + batch read_pages + post-write persist", N_PAGES);

    // ── Phase 1: Write multi-page nonzero pattern, fsync, close ──────
    {
        let fd = sys_open(TEST_PATH, O_CREAT | O_RDWR | O_TRUNC);
        if fd < 0 {
            println!("FAIL: phase 1 create returned {}", fd);
            return 1;
        }
        let f = fd as usize;

        for page in 0..N_PAGES {
            let val = 0xA0u8.wrapping_add(page as u8);
            let buf = [val; PAGE_SIZE];
            let n = sys_write(f, &buf);
            if n != PAGE_SIZE as isize {
                println!("FAIL: phase 1 write page {} returned {} (expected {})", page, n, PAGE_SIZE);
                let _ = sys_close(f);
                return 1;
            }
        }
        println!("  phase 1: wrote {} pages 0x{:02x}..0x{:02x}",
                 N_PAGES, 0xA0u8, 0xA0u8.wrapping_add((N_PAGES - 1) as u8));

        let ret = sys_fsync(f);
        if ret < 0 {
            println!("FAIL: phase 1 fsync returned {}", ret);
            let _ = sys_close(f);
            return 1;
        }

        let ret = sys_close(f);
        if ret < 0 {
            println!("FAIL: phase 1 close returned {}", ret);
            return 1;
        }
        println!("  phase 1: synced and closed");
    }

    // ── Phase 2: Reopen, shrink to one page, fsync, close ────────────
    {
        let fd = sys_open(TEST_PATH, O_RDWR);
        if fd < 0 {
            println!("FAIL: phase 2 reopen returned {}", fd);
            return 1;
        }
        let f = fd as usize;

        let ret = sys_ftruncate(f, PAGE_SIZE as isize);
        if ret < 0 {
            println!("FAIL: phase 2 ftruncate shrink returned {}", ret);
            let _ = sys_close(f);
            return 1;
        }
        println!("  phase 2: truncated to 1 page");

        let ret = sys_fsync(f);
        if ret < 0 {
            println!("FAIL: phase 2 fsync returned {}", ret);
            let _ = sys_close(f);
            return 1;
        }

        let ret = sys_close(f);
        if ret < 0 {
            println!("FAIL: phase 2 close returned {}", ret);
            return 1;
        }
        println!("  phase 2: synced and closed");
    }

    // ── Phase 3: Cold reopen, extend, exercise read_page + read_pages ─
    // After close, PageCache is invalidated; next reads must go through
    // the backend. Page 0 is within physical EOF; pages 1+ are sparse holes.
    {
        let fd = sys_open(TEST_PATH, O_RDWR);
        if fd < 0 {
            println!("FAIL: phase 3 cold reopen returned {}", fd);
            return 1;
        }
        let f = fd as usize;

        // Extend back to original size (creates hole from PAGE_SIZE to FILE_SIZE)
        let ret = sys_ftruncate(f, FILE_SIZE as isize);
        if ret < 0 {
            println!("FAIL: phase 3 ftruncate extend returned {}", ret);
            let _ = sys_close(f);
            return 1;
        }
        println!("  phase 3: extended back to {} pages (cold reopen)", N_PAGES);

        // 3a — Page 0: single page, within physical EOF, must retain 0xA0
        if verify_page(f, 0, 0xA0u8, "phase 3a: page-0 (within eof)") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 3b — Page 1: single-page read, strictly beyond physical EOF.
        // Exercises LwExt4PageCacheBackend::read_page for a sparse hole.
        if verify_page(f, PAGE_SIZE as isize, 0u8, "phase 3b: page-1 single (read_page)") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 3c — Pages 1-2: 8192-byte batch read spanning two sparse pages.
        // Exercises LwExt4PageCacheBackend::read_pages for batch hole reads.
        if verify_batch(f, PAGE_SIZE as isize, TWO_PAGES, 0u8,
                        "phase 3c: pages-1-2 batch (read_pages)") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 3d — Write marker 0xBB to page 3 (a hole beyond physical EOF),
        // then fsync+close so the write reaches disk.
        {
            let marker: u8 = 0xBB;
            let wbuf = [marker; PAGE_SIZE];
            let pos = sys_lseek(f, (3 * PAGE_SIZE) as isize, SEEK_SET);
            if pos != (3 * PAGE_SIZE) as isize {
                println!("FAIL: phase 3d lseek page 3 returned {}", pos);
                let _ = sys_close(f);
                return 1;
            }
            let n = sys_write(f, &wbuf);
            if n != PAGE_SIZE as isize {
                println!("FAIL: phase 3d write marker page 3 returned {} (expected {})", n, PAGE_SIZE);
                let _ = sys_close(f);
                return 1;
            }
            println!("  phase 3d: wrote marker 0x{:02x} to page 3", marker);
        }

        let ret = sys_fsync(f);
        if ret < 0 {
            println!("FAIL: phase 3 fsync returned {}", ret);
            let _ = sys_close(f);
            return 1;
        }
        let ret = sys_close(f);
        if ret < 0 {
            println!("FAIL: phase 3 close returned {}", ret);
            return 1;
        }
        println!("  phase 3: synced and closed");
    }

    // ── Phase 4: Cold reopen, verify post-write persistence ──────────
    // Marker on page 3 must survive close→reopen. Adjacent unmodified
    // hole pages (2, 4) must remain zero; page 0 original data intact.
    {
        let fd = sys_open(TEST_PATH, O_RDWR);
        if fd < 0 {
            println!("FAIL: phase 4 cold reopen returned {}", fd);
            return 1;
        }
        let f = fd as usize;

        // 4a — Page 3: marker must persist after close→reopen
        if verify_page(f, (3 * PAGE_SIZE) as isize, 0xBBu8, "phase 4a: page-3 marker persist") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 4b — Page 2: unmodified hole adjacent to marker, must be zeros
        if verify_page(f, (2 * PAGE_SIZE) as isize, 0u8, "phase 4b: page-2 adj hole") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 4c — Page 4: unmodified hole on the other side of marker, must be zeros
        if verify_page(f, (4 * PAGE_SIZE) as isize, 0u8, "phase 4c: page-4 adj hole") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        // 4d — Page 0: original data still intact
        if verify_page(f, 0, 0xA0u8, "phase 4d: page-0 intact") != 0 {
            let _ = sys_close(f);
            return 1;
        }

        let ret = sys_close(f);
        if ret < 0 {
            println!("FAIL: phase 4 close returned {}", ret);
            return 1;
        }
        println!("  phase 4: verified, closed");
    }

    // ── Cleanup (best-effort) ────────────────────────────────────────
    {
        let ret = sys_unlinkat(AT_FDCWD, TEST_PATH, 0);
        if ret < 0 {
            println!("  cleanup: unlink returned {} (non-fatal)", ret);
        } else {
            println!("  cleanup: test file removed");
        }
    }

    println!("[regression_lwext4_trunc_hole] PASS");
    0
}
