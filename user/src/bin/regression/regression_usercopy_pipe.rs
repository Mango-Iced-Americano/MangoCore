//! Regression: failed copy to/from user consumed pipe data
//! Bug: When read from pipe fails due to bad user buffer (EFAULT),
//!      the pipe data was already consumed (lost).
//! Expected: Read with bad buffer returns -EFAULT, pipe data
//!           still available on next valid read.
//! Related subsystem: pipe / usercopy
//! LTP counterpart: none (found by Oracle audit)
//! Source: docs/02_syscall/fs-fd-event.md (§5.1 read/write user buffer)

use user_lib::syscall::*;
use user_lib::println;

const EFAULT: isize = -14;
// 两个竞赛架构都使用 4 KiB 基础页；回归程序不依赖 LA64 专用的用户布局模块。
const PAGE_SIZE: usize = 0x1000;
const PROT_READ: usize = 1;
const PROT_READ_WRITE: usize = 3;
const MAP_PRIVATE: usize = 2;
const MAP_ANONYMOUS: usize = 0x20;

/// 验证跨页 read 只消费当前可写前缀，不能为了后一页提前丢失 pipe 数据。
fn partial_cross_page_read() -> bool {
    let mut pipefd = [-1i32; 2];
    if sys_pipe(&mut pipefd) < 0 {
        return false;
    }
    let (rfd, wfd) = (pipefd[0] as usize, pipefd[1] as usize);
    let mapping = sys_mmap(
        0,
        PAGE_SIZE * 2,
        PROT_READ_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        0,
        0,
    );
    if mapping < 0 {
        let _ = sys_close(rfd);
        let _ = sys_close(wfd);
        return false;
    }

    let base = mapping as usize;
    let prefix = base + PAGE_SIZE - 8;
    // 先触页，确保前一页已有可写 PTE；后一页降为只读，形成精确前缀边界。
    unsafe { (prefix as *mut u8).write_volatile(0); }
    let protected = sys_mprotect(base + PAGE_SIZE, PAGE_SIZE, PROT_READ) == 0;
    let payload = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
    ];
    let written = protected && sys_write(wfd, &payload) == payload.len() as isize;
    // 关闭生产端，使“错误地多消费数据”表现为 EOF，而不是让回归永久阻塞。
    let _ = sys_close(wfd);
    let first = if written {
        sys_read_raw(rfd, prefix as *mut u8, payload.len())
    } else {
        EFAULT
    };
    let prefix_ok = first == 8
        && unsafe { core::slice::from_raw_parts(prefix as *const u8, 8) } == &payload[..8];

    let mut tail = [0u8; 8];
    let second = if prefix_ok {
        sys_read(rfd, &mut tail)
    } else {
        EFAULT
    };
    let tail_ok = second == 8 && tail == payload[8..];
    println!(
        "  cross-page detail: protected={} first={} prefix_ok={} second={} tail_ok={}",
        protected, first, prefix_ok, second, tail_ok
    );

    let _ = sys_munmap(base, PAGE_SIZE * 2);
    let _ = sys_close(rfd);
    protected && written && prefix_ok && tail_ok
}

pub fn run() -> i32 {
    println!("[regression_usercopy_pipe] start");

    // 1. Create pipe
    let mut pipefd: [i32; 2] = [-1, -1];
    let ret = sys_pipe(&mut pipefd);
    if ret < 0 {
        println!("FAIL: pipe() returned {}", ret);
        return 1;
    }
    let (rfd, wfd) = (pipefd[0] as usize, pipefd[1] as usize);
    println!("  pipe created: rfd={} wfd={}", rfd, wfd);

    // 2. Write known data to pipe
    let sent: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22];
    let n = sys_write(wfd, &sent);
    if n != 8 {
        println!("FAIL: write returned {} (expected 8)", n);
        return 1;
    }
    println!("  wrote {} bytes to pipe", n);

    // 3. Read with NULL buffer → should get -EFAULT, data NOT consumed
    let bad_ret = sys_read_raw(rfd, core::ptr::null_mut(), 8);
    println!("  read(null, 8) returned {} (expect {})", bad_ret, EFAULT);
    if bad_ret != EFAULT {
        println!("FAIL: unexpected return value from bad-buffer read");
        return 1;
    }

    // 4. Read again with valid buffer — should get the same data (not lost)
    let mut recv: [u8; 8] = [0; 8];
    let n2 = sys_read(rfd, &mut recv);
    if n2 != 8 {
        println!("FAIL: second read returned {} (expected 8)", n2);
        if n2 == 0 {
            println!("  → DATA LOST! Bug reproduced: pipe data consumed by failed read.");
        }
        return 1;
    }
    println!("  second read returned {} bytes", n2);

    // Verify data matches
    if recv != sent {
        println!("FAIL: data mismatch sent={:?} recv={:?}", sent, recv);
        return 1;
    }

    let _ = sys_close(rfd);
    let _ = sys_close(wfd);

    if !partial_cross_page_read() {
        println!("FAIL: cross-page read consumed beyond writable prefix");
        return 1;
    }

    println!("[regression_usercopy_pipe] PASS");
    0
}
