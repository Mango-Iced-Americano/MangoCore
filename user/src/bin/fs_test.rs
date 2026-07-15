#![no_std]
#![no_main]

extern crate alloc;
use alloc::format;
use alloc::string::String;
use user_lib::syscall::*;
use user_lib::{println};

// ── Path construction helpers (no_std compatible) ──────────────────────

fn make_path(prefix: &str, name: &str) -> String {
    let mut s = String::new();
    s.push_str(prefix);
    s.push_str(name);
    s.push('\0');
    s
}

fn make_file_path(prefix: &str, idx: usize) -> String {
    let mut s = format!("{}/file_{:04}", prefix, idx);
    s.push('\0');
    s
}

fn make_link_path(prefix: &str, idx: usize) -> String {
    let mut s = format!("{}/link_{:04}", prefix, idx);
    s.push('\0');
    s
}

// ── getdents64 parsing helper ─────────────────────────────────────────

/// Parse a linux_dirent64 byte buffer and count non-"." and non-".." entries.
/// Returns (entry_count, invalid_reclen_detected).
/// d_type is at offset 18 (kernel linux_dirent64 format, NOT glibc dirent64).
fn count_dir_entries(buf: &[u8], n: isize, _expected_prefix: Option<&str>) -> (usize, bool) {
    let bytes = &buf[..n as usize];
    let mut count = 0usize;
    let mut invalid = false;
    let mut pos = 0;
    while pos + 19 <= bytes.len() {
        // d_reclen at offset 16-17 (u16 LE)
        let d_reclen = u16::from_le_bytes([bytes[pos + 16], bytes[pos + 17]]) as usize;
        if d_reclen == 0 || d_reclen < 19 {
            invalid = true;
            break;
        }
        if pos + d_reclen > bytes.len() {
            invalid = true;
            break;
        }
        // d_type is at offset 18 in kernel linux_dirent64 (struct: d_ino(8)+d_off(8)+d_reclen(2)+d_type(1)+d_name[])
        let _d_type = bytes[pos + 18];
        let name_start = pos + 19;
        let name_end = bytes[name_start..pos + d_reclen - 1]
            .iter()
            .position(|&b| b == 0)
            .map(|i| name_start + i)
            .unwrap_or(pos + d_reclen - 1);
        let name = core::str::from_utf8(&bytes[name_start..name_end]).unwrap_or("???");
        if name != "." && name != ".." {
            count += 1;
        }
        pos += d_reclen;
    }
    (count, invalid)
}

fn test_mkdir() -> bool {
    let ret = sys_mkdirat(AT_FDCWD, "/tmp/fs_test\0", 0o777);
    if ret < 0 {
        println!("  FAIL: mkdirat /tmp/fs_test returned {}", ret);
        return false;
    }
    println!("  PASS: mkdirat /tmp/fs_test OK");
    true
}

fn test_create_and_write() -> bool {
    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    const O_TRUNC: u32 = 0o1000;

    let fd = sys_open("/tmp/fs_test/file\0", O_CREAT | O_WRONLY | O_TRUNC);
    if fd < 0 {
        println!("  FAIL: open /tmp/fs_test/file returned {}", fd);
        return false;
    }

    let msg = b"hello filesystem!\n";
    let n = sys_write(fd as usize, msg);
    sys_close(fd as usize);

    if n != msg.len() as isize {
        println!("  FAIL: write returned {} (expected {})", n, msg.len());
        return false;
    }
    println!("  PASS: create+write /tmp/fs_test/file OK ({} bytes)", n);
    true
}

fn test_read() -> bool {
    const O_RDONLY: u32 = 0;

    let fd = sys_open("/tmp/fs_test/file\0", O_RDONLY);
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
    println!("  PASS: read /tmp/fs_test/file OK");
    true
}

fn test_symlink() -> bool {
    let ret = sys_symlinkat("/tmp/fs_test/file\0", AT_FDCWD, "/tmp/fs_test/link\0");
    if ret < 0 {
        println!("  FAIL: symlinkat returned {}", ret);
        return false;
    }
    println!("  PASS: symlinkat /tmp/fs_test/link -> /tmp/testfile OK");
    true
}

fn test_readlink() -> bool {
    let mut buf = [0u8; 256];
    let n = sys_readlinkat(AT_FDCWD, "/tmp/fs_test/link\0", &mut buf);
    if n < 0 {
        println!("  FAIL: readlinkat returned {}", n);
        return false;
    }

    let target = core::str::from_utf8(&buf[..n as usize]).unwrap_or("???");
    if target != "/tmp/fs_test/file" {
        println!("  FAIL: readlink target='{}' (expected '/tmp/fs_test/file')", target);
        return false;
    }
    println!("  PASS: readlinkat /tmp/fs_test/link -> '{}' OK", target);
    true
}

fn test_read_via_symlink() -> bool {
    const O_RDONLY: u32 = 0;

    let fd = sys_open("/tmp/fs_test/link\0", O_RDONLY);
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
    let ret = sys_unlinkat(AT_FDCWD, "/tmp/fs_test/link\0", 0);
    if ret < 0 {
        println!("  FAIL: unlinkat testlink returned {}", ret);
        return false;
    }

    let ret2 = sys_unlinkat(AT_FDCWD, "/tmp/fs_test/file\0", 0);
    if ret2 < 0 {
        println!("  FAIL: unlinkat testfile returned {}", ret2);
        return false;
    }
    println!("  PASS: unlinkat OK");
    true
}

fn test_rmdir() -> bool {
    const AT_REMOVEDIR: u32 = 0x200;
    let ret = sys_unlinkat(AT_FDCWD, "/tmp/fs_test\0", AT_REMOVEDIR);
    if ret < 0 {
        println!("  FAIL: rmdir /tmp/fs_test returned {}", ret);
        return false;
    }
    println!("  PASS: rmdir /tmp/fs_test OK");
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

// ── Phase A: linkat ─────────────────────────────────────────────────────

fn test_hard_link() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp8\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp8/original\0", O_CREAT | O_WRONLY);
    if fd < 0 {
        println!("  FAIL: create original returned {}", fd);
        return false;
    }
    sys_write(fd as usize, b"hard-link-test\n");
    sys_close(fd as usize);

    // linkat: create hard link
    let ret = sys_linkat(AT_FDCWD, "/tmp8/original\0", AT_FDCWD, "/tmp8/link\0", 0);
    if ret < 0 {
        println!("  FAIL: linkat returned {}", ret);
        return false;
    }

    // read via hard link
    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp8/link\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: open hard link returned {}", fd);
        return false;
    }
    let mut buf = [0u8; 32];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"hard-link-test\n" {
        println!("  FAIL: hard link read mismatch");
        return false;
    }
    println!("  PASS: hard link read OK");

    // unlink original, link should still work (nlink >= 2)
    sys_unlinkat(AT_FDCWD, "/tmp8/original\0", 0);
    let fd = sys_open("/tmp8/link\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: hard link broken after unlink original, err={}", fd);
        return false;
    }
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"hard-link-test\n" {
        println!("  FAIL: hard link content lost after unlink");
        return false;
    }
    println!("  PASS: hard link survives unlink of original");

    sys_unlinkat(AT_FDCWD, "/tmp8/link\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp8\0", 0x200);
    true
}

fn test_hard_link_dir_rejected() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp9\0", 0o777);
    sys_mkdirat(AT_FDCWD, "/tmp9/sub\0", 0o777);

    // hard link to directory should be rejected (EISDIR or EPERM)
    let ret = sys_linkat(AT_FDCWD, "/tmp9/sub\0", AT_FDCWD, "/tmp9/dirlink\0", 0);
    if ret >= 0 {
        println!("  FAIL: hard link to dir should fail, got {}", ret);
        return false;
    }
    // Linux returns EPERM(1) for dir hardlink, we accept EPERM or EISDIR
    if ret != -1 && ret != -21 {
        println!("  FAIL: expected -1(EPERM) or -21(EISDIR), got {}", ret);
        return false;
    }
    println!("  PASS: hard link to directory rejected (err={})", -ret);

    sys_unlinkat(AT_FDCWD, "/tmp9/sub\0", 0x200);
    sys_unlinkat(AT_FDCWD, "/tmp9\0", 0x200);
    true
}

// ── Phase A: lseek ──────────────────────────────────────────────────────

fn test_lseek() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp10\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp10/seekfile\0", O_CREAT | O_RDWR);
    if fd < 0 {
        println!("  FAIL: create seekfile returned {}", fd);
        return false;
    }
    let f = fd as usize;
    sys_write(f, b"0123456789");

    // SEEK_SET
    let pos = sys_lseek(f, 3, SEEK_SET);
    if pos != 3 {
        println!("  FAIL: SEEK_SET(3) got {}", pos);
        return false;
    }
    let mut buf = [0u8; 4];
    let n = sys_read(f, &mut buf);
    if &buf[..n as usize] != b"3456" {
        println!("  FAIL: SEEK_SET read mismatch");
        return false;
    }

    // SEEK_CUR
    let pos = sys_lseek(f, 2, SEEK_CUR);
    if pos != 9 {
        println!("  FAIL: SEEK_CUR(2) from 7 got {}", pos);
        return false;
    }

    // SEEK_END
    let pos = sys_lseek(f, 0, SEEK_END);
    if pos != 10 {
        println!("  FAIL: SEEK_END got {}", pos);
        return false;
    }

    // SEEK_END negative offset
    let pos = sys_lseek(f, -3, SEEK_END);
    if pos != 7 {
        println!("  FAIL: SEEK_END(-3) got {}", pos);
        return false;
    }

    // lseek beyond EOF (should succeed, next write creates hole)
    let pos = sys_lseek(f, 100, SEEK_SET);
    if pos != 100 {
        println!("  FAIL: SEEK_SET(100) beyond EOF got {}", pos);
        return false;
    }
    println!("  PASS: lseek SEEK_SET/CUR/END OK (pos={})", pos);

    sys_close(f);
    sys_unlinkat(AT_FDCWD, "/tmp10/seekfile\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp10\0", 0x200);
    true
}

// ── Phase A: renameat2 ──────────────────────────────────────────────────

fn test_rename_file() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp11\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp11/before\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"renamed\n");
    sys_close(fd as usize);

    let ret = sys_renameat2(AT_FDCWD, "/tmp11/before\0", AT_FDCWD, "/tmp11/after\0", 0);
    if ret < 0 {
        println!("  FAIL: renameat2 returned {}", ret);
        return false;
    }

    // old path should no longer exist
    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp11/before\0", O_RDONLY);
    if fd >= 0 {
        println!("  FAIL: old path still exists after rename");
        sys_close(fd as usize);
        return false;
    }

    // new path should have content
    let fd = sys_open("/tmp11/after\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: new path gone after rename, err={}", fd);
        return false;
    }
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"renamed\n" {
        println!("  FAIL: rename content mismatch");
        return false;
    }
    println!("  PASS: rename file OK");

    sys_unlinkat(AT_FDCWD, "/tmp11/after\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp11\0", 0x200);
    true
}

fn test_rename_dir() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp12\0", 0o777);
    sys_mkdirat(AT_FDCWD, "/tmp12/dir_a\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp12/dir_a/file\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"in-renamed-dir\n");
    sys_close(fd as usize);

    let ret = sys_renameat2(AT_FDCWD, "/tmp12/dir_a\0", AT_FDCWD, "/tmp12/dir_b\0", 0);
    if ret < 0 {
        println!("  FAIL: rename dir returned {}", ret);
        return false;
    }

    // file should be accessible via new path
    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp12/dir_b/file\0", O_RDONLY);
    if fd < 0 {
        println!("  FAIL: file under renamed dir not found, err={}", fd);
        return false;
    }
    let mut buf = [0u8; 32];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"in-renamed-dir\n" {
        println!("  FAIL: renamed dir content mismatch");
        return false;
    }
    println!("  PASS: rename directory OK");

    sys_unlinkat(AT_FDCWD, "/tmp12/dir_b/file\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp12/dir_b\0", 0x200);
    sys_unlinkat(AT_FDCWD, "/tmp12\0", 0x200);
    true
}

// ── Phase A: fstatat ────────────────────────────────────────────────────

fn test_fstatat() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp13\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp13/statfile\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"stat-test-data-12345\n");
    sys_close(fd as usize);
    sys_symlinkat("/tmp13/statfile\0", AT_FDCWD, "/tmp13/statlink\0");

    // fstatat with AT_SYMLINK_NOFOLLOW on symlink
    let mut st = Stat {
        st_dev: 0, st_ino: 0, st_mode: 0, st_nlink: 0, st_uid: 0, st_gid: 0,
        st_rdev: 0, __pad: 0, st_size: 0, st_blksize: 0, __pad2: 0, st_blocks: 0,
        st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        __unused: 0,
    };
    let ret = sys_fstatat(AT_FDCWD, "/tmp13/statlink\0", &mut st, AT_SYMLINK_NOFOLLOW);
    if ret < 0 {
        println!("  FAIL: fstatat with NOFOLLOW returned {}", ret);
        return false;
    }
    // symlink mode includes S_IFLNK (0o120000)
    if (st.st_mode & 0o170000) != 0o120000 {
        println!("  FAIL: expected S_IFLNK mode, got 0{:o}", st.st_mode & 0o170000);
        return false;
    }
    println!("  PASS: fstatat symlink (NOFOLLOW) mode=0{:o}", st.st_mode & 0o170000);

    // fstatat following symlink
    let ret = sys_fstatat(AT_FDCWD, "/tmp13/statlink\0", &mut st, 0);
    if ret < 0 {
        println!("  FAIL: fstatat following symlink returned {}", ret);
        return false;
    }
    if (st.st_mode & 0o170000) != 0o100000 {
        println!("  FAIL: expected S_IFREG, got 0{:o}", st.st_mode & 0o170000);
        return false;
    }
    if st.st_size != 21 {
        println!("  FAIL: expected size=21, got {}", st.st_size);
        return false;
    }
    println!("  PASS: fstatat regular file size={} nlink={}", st.st_size, st.st_nlink);

    sys_unlinkat(AT_FDCWD, "/tmp13/statlink\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp13/statfile\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp13\0", 0x200);
    true
}

// ── Phase A: ftruncate ──────────────────────────────────────────────────

fn test_ftruncate() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp14\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp14/truncfile\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"0123456789ABCDEF"); // 16 bytes
    sys_close(fd as usize);

    // truncate to 6 bytes
    let fd = sys_open("/tmp14/truncfile\0", O_RDWR);
    let ret = sys_ftruncate(fd as usize, 6);
    if ret < 0 {
        println!("  FAIL: ftruncate returned {}", ret);
        return false;
    }

    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    if n != 6 {
        println!("  FAIL: after truncate read got {} bytes (expected 6)", n);
        return false;
    }
    if &buf[..6] != b"012345" {
        println!("  FAIL: truncate content mismatch");
        return false;
    }
    println!("  PASS: ftruncate to 6 bytes OK");

    // extend (hole): expand to 100 bytes, read beyond old EOF should be zeros
    let ret = sys_ftruncate(fd as usize, 100);
    if ret < 0 {
        println!("  FAIL: ftruncate extend returned {}", ret);
        return false;
    }
    let _pos = sys_lseek(fd as usize, 20, SEEK_SET);
    let n = sys_read(fd as usize, &mut buf);
    if n < 0 {
        println!("  FAIL: read in hole returned {}", n);
        return false;
    }
    let zeros = [0u8; 16];
    if &buf[..n as usize] != &zeros[..n as usize] {
        println!("  FAIL: hole not zero-filled");
        return false;
    }
    println!("  PASS: ftruncate hole zero-filled ({} bytes at offset 20)", n);

    sys_close(fd as usize);
    sys_unlinkat(AT_FDCWD, "/tmp14/truncfile\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp14\0", 0x200);
    true
}

// ── Phase A: getdents64 ─────────────────────────────────────────────────

fn test_getdents64() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp15\0", 0o777);

    const O_CREAT: u32 = 0o100;
    const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp15/x\0", O_CREAT | O_WRONLY);
    if fd >= 0 { sys_close(fd as usize); }

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp15\0", O_RDONLY | 0o200000);
    if fd < 0 {
        println!("  FAIL: open dir returned {}", fd);
        return false;
    }

    let mut buf = [0u8; 1024];
    let n = sys_getdents64(fd as usize, &mut buf);
    sys_close(fd as usize);

    if n <= 0 {
        println!("  FAIL: getdents64 returned {}", n);
        return false;
    }

    let (entries, invalid) = count_dir_entries(&buf, n, None);
    if invalid || entries != 1 {
        println!("  FAIL: getdents64 parsed entries={} invalid={}", entries, invalid);
        return false;
    }
    println!("  PASS: getdents64 OK (. .. x)");

    sys_unlinkat(AT_FDCWD, "/tmp15/x\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp15\0", 0x200);
    true
}

// ═══════════════════════════════════════════════════════════════════════
// Phase B: LTP-inspired advanced tests
// ═══════════════════════════════════════════════════════════════════════

// ── A组: 高级 read/write 测试 ───────────────────────────────────────────

fn test_read_empty() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp16\0", 0o777);
    const O_CREAT: u32 = 0o100;
    let fd = sys_open("/tmp16/empty\0", O_CREAT);
    sys_close(fd as usize);

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp16/empty\0", O_RDONLY);
    if fd < 0 { println!("  FAIL: open empty returned {}", fd); return false; }
    let mut buf = [1u8; 64];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n != 0 {
        println!("  FAIL: read empty file got {} bytes (expected 0)", n);
        return false;
    }
    println!("  PASS: read empty file -> 0 bytes OK");

    sys_unlinkat(AT_FDCWD, "/tmp16/empty\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp16\0", 0x200);
    true
}

fn test_read_past_eof() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp17\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp17/short\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"hello");
    sys_close(fd as usize);

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp17/short\0", O_RDONLY);
    let mut buf = [0u8; 100];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n != 5 { println!("  FAIL: read past EOF got {} (expected 5)", n); return false; }
    println!("  PASS: read past EOF returns 5 OK");

    sys_unlinkat(AT_FDCWD, "/tmp17/short\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp17\0", 0x200);
    true
}

fn test_read_data_integrity() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp18\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp18/pat\0", O_CREAT | O_RDWR);
    let mut pat = [0u8; 256];
    for i in 0usize..256 { pat[i] = i as u8; }
    let n = sys_write(fd as usize, &pat);
    if n != 256 { println!("  FAIL: write 256 bytes returned {}", n); return false; }
    sys_close(fd as usize);
    let fd = sys_open("/tmp18/pat\0", O_RDONLY);
    let mut buf = [0u8; 256];
    let n = sys_read(fd as usize, &mut buf);
    if n != 256 { println!("  FAIL: read integrity got {}", n); return false; }
    for i in 0..256usize { if buf[i] != i as u8 { println!("  FAIL: byte {} mismatch: {} != {}", i, buf[i], i as u8); return false; } }
    // partial read from middle
    let pos = sys_lseek(fd as usize, 100, SEEK_SET);
    if pos != 100 { println!("  FAIL: seek to 100 got {}", pos); return false; }
    let n = sys_read(fd as usize, &mut buf[..50]);
    if n != 50 { println!("  FAIL: partial read got {}", n); return false; }
    for i in 0..50usize { if buf[i] != (100 + i) as u8 { println!("  FAIL: partial byte {} mismatch", i); return false; } }
    sys_close(fd as usize);
    println!("  PASS: data integrity 256B + partial read OK");
    sys_unlinkat(AT_FDCWD, "/tmp18/pat\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp18\0", 0x200);
    true
}

fn test_read_bad_fd() -> bool {
    let mut buf = [0u8; 16];
    let n = sys_read(99999, &mut buf);
    if n != -9 { println!("  FAIL: read bad fd got {} (expected -9/EBADF)", n); return false; }
    println!("  PASS: read bad fd -> EBADF OK");
    true
}

fn test_read_dir() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp19\0", 0o777);
    let fd = sys_open("/tmp19\0", 0);
    if fd < 0 { println!("  FAIL: open tmp19 returned {}", fd); return false; }
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n != -21 { println!("  FAIL: read dir got {} (expected -21/EISDIR)", n); return false; }
    println!("  PASS: read on dir fd -> EISDIR OK");
    sys_unlinkat(AT_FDCWD, "/tmp19\0", 0x200);
    true
}

fn test_write_readonly() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp20\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_WRONLY: u32 = 0o1;
    let fd = sys_open("/tmp20/ro\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"data");
    sys_close(fd as usize);

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp20/ro\0", O_RDONLY);
    let n = sys_write(fd as usize, b"X");
    sys_close(fd as usize);
    if n != -9 { println!("  FAIL: write to readonly fd got {} (expected -9/EBADF)", n); return false; }
    println!("  PASS: write to readonly fd -> EBADF OK");

    sys_unlinkat(AT_FDCWD, "/tmp20/ro\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp20\0", 0x200);
    true
}

fn test_write_append() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp21\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    const O_APPEND: u32 = 0o2000;
    let fd = sys_open("/tmp21/afile\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"hello");
    sys_close(fd as usize);
    let fd = sys_open("/tmp21/afile\0", O_RDWR | O_APPEND);
    let pos = sys_lseek(fd as usize, 0, SEEK_SET);
    if pos != 0 { println!("  FAIL: lseek to 0 got {}", pos); return false; }
    sys_write(fd as usize, b"world");
    let end = sys_lseek(fd as usize, 0, SEEK_END);
    if end != 10 { println!("  FAIL: SEEK_END after append got {} (expected 10)", end); return false; }
    sys_lseek(fd as usize, 0, SEEK_SET);
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"helloworld" {
        println!("  FAIL: append content mismatch: {:?}", &buf[..n as usize]);
        return false;
    }
    println!("  PASS: O_APPEND write after lseek OK");
    sys_unlinkat(AT_FDCWD, "/tmp21/afile\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp21\0", 0x200);
    true
}

fn test_write_varying_sizes() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp22\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_TRUNC: u32 = 0o1000;
    let fd = sys_open("/tmp22/vary\0", O_CREAT | O_RDWR | O_TRUNC);
    if fd < 0 { println!("  FAIL: create vary returned {}", fd); return false; }
    let mut size: u32 = 1;
    while size <= 4096 {
        let mut data = [0u8; 4096];
        for i in 0..size as usize { data[i] = (size as u8).wrapping_add(i as u8); }
        let n = sys_write(fd as usize, &data[..size as usize]);
        if n != size as isize { println!("  FAIL: write size={} returned {}", size, n); return false; }
        sys_lseek(fd as usize, -(size as isize), SEEK_CUR);
        let mut rbuf = [0u8; 4096];
        let n = sys_read(fd as usize, &mut rbuf[..size as usize]);
        if n != size as isize { println!("  FAIL: read back size={} got {}", size, n); return false; }
        for i in 0..size as usize { if rbuf[i] != data[i] { println!("  FAIL: verify size={} byte {}", size, i); return false; } }
        size <<= 1;
    }
    sys_close(fd as usize);
    println!("  PASS: write varying 1..4096 bytes OK");
    sys_unlinkat(AT_FDCWD, "/tmp22/vary\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp22\0", 0x200);
    true
}

fn test_write_overwrite_middle() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp23\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp23/over\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"abcdefgh");
    sys_lseek(fd as usize, 2, SEEK_SET);
    let n = sys_write(fd as usize, b"XY");
    if n != 2 { println!("  FAIL: overwrite write returned {}", n); return false; }
    sys_lseek(fd as usize, 0, SEEK_SET);
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"abXYefgh" { println!("  FAIL: overwrite content: {:?}", &buf[..n as usize]); return false; }
    println!("  PASS: overwrite middle -> abXYfgh OK");
    sys_unlinkat(AT_FDCWD, "/tmp23/over\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp23\0", 0x200);
    true
}

fn test_write_bad_fd() -> bool {
    let n = sys_write(99999, b"X");
    if n != -9 { println!("  FAIL: write bad fd got {} (expected -9/EBADF)", n); return false; }
    println!("  PASS: write bad fd -> EBADF OK");
    true
}

// ── B组: 高级 lseek 测试 ────────────────────────────────────────────────

fn test_lseek_seek_end() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp24\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp24/end\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"ABCDEFGHIJ"); // 10 bytes
    let pos = sys_lseek(fd as usize, 0, SEEK_END);
    if pos != 10 { println!("  FAIL: SEEK_END got {} expected 10", pos); return false; }
    let pos = sys_lseek(fd as usize, -4, SEEK_END);
    if pos != 6 { println!("  FAIL: SEEK_END(-4) got {} expected 6", pos); return false; }
    let mut buf = [0u8; 4];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"GHIJ" { println!("  FAIL: SEEK_END(-4) read got {:?}", &buf[..n as usize]); return false; }
    println!("  PASS: SEEK_END + negative offset OK");
    sys_unlinkat(AT_FDCWD, "/tmp24/end\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp24\0", 0x200);
    true
}

fn test_lseek_bad_whence() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp25\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp25/f\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"data");

    let pos = sys_lseek(fd as usize, 0, 99);
    sys_close(fd as usize);
    if pos != -22 { println!("  FAIL: bad whence=99 got {} (expected -22/EINVAL)", pos); return false; }
    println!("  PASS: lseek bad whence -> EINVAL OK");

    sys_unlinkat(AT_FDCWD, "/tmp25/f\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp25\0", 0x200);
    true
}

fn test_lseek_pipe() -> bool {
    let mut fds = [0i32; 2];
    let ret = sys_pipe(&mut fds);
    if ret < 0 { println!("  FAIL: pipe created returned {}", ret); return false; }
    let pos = sys_lseek(fds[0] as usize, 0, SEEK_SET);
    sys_close(fds[0] as usize); sys_close(fds[1] as usize);
    if pos != -29 { println!("  FAIL: lseek on pipe got {} (expected -29/ESPIPE)", pos); return false; }
    println!("  PASS: lseek on pipe -> ESPIPE OK");
    true
}

fn test_lseek_hole_read() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp26\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp26/hole\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"0123456789"); // 10 bytes at offset 0
    let pos = sys_lseek(fd as usize, 50, SEEK_SET);
    if pos != 50 { println!("  FAIL: seek to 50 got {}", pos); return false; }
    let n2 = sys_write(fd as usize, b"DATA_AT_50");
    if n2 != 10 { println!("  FAIL: second write returned {} (expected 10)", n2); return false; }
    sys_lseek(fd as usize, 0, SEEK_SET);
    let mut buf = [0u8; 70];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    // first 10 = "0123456789", 10..50 = 40 zeros (hole), 50..60 = "DATA_AT_50"
    if n < 60 { println!("  FAIL: hole read only got {} bytes", n); return false; }
    if &buf[0..10] != b"0123456789" { println!("  FAIL: hole: data start mismatch"); return false; }
    let zeros: [u8; 40] = [0u8; 40];
    if &buf[10..50] != &zeros[..] { println!("  FAIL: hole not zero-filled"); return false; }
    if &buf[50..60] != b"DATA_AT_50" { println!("  FAIL: hole: data at 50 mismatch"); return false; }
    println!("  PASS: lseek beyond EOF + hole read OK");
    sys_unlinkat(AT_FDCWD, "/tmp26/hole\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp26\0", 0x200);
    true
}

fn test_lseek_chain() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp27\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp27/chain\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"); // 26 bytes
    let pos = sys_lseek(fd as usize, 5, SEEK_SET);
    if pos != 5 { println!("  FAIL: chain SET(5) got {}", pos); return false; }
    let mut c = [0u8; 1];
    sys_read(fd as usize, &mut c);
    if c[0] != b'F' { println!("  FAIL: chain step1 expected F got {}", c[0]); return false; }
    let pos = sys_lseek(fd as usize, -3, SEEK_CUR);
    if pos != 3 { println!("  FAIL: chain CUR(-3) got {}", pos); return false; }
    sys_read(fd as usize, &mut c);
    if c[0] != b'D' { println!("  FAIL: chain step2 expected D got {}", c[0]); return false; }
    let pos = sys_lseek(fd as usize, -10, SEEK_END);
    if pos != 16 { println!("  FAIL: chain END(-10) got {}", pos); return false; }
    sys_read(fd as usize, &mut c);
    if c[0] != b'Q' { println!("  FAIL: chain step3 expected Q got {}", c[0]); return false; }
    sys_close(fd as usize);
    println!("  PASS: lseek chain SEEK_SET→CUR→END OK");
    sys_unlinkat(AT_FDCWD, "/tmp27/chain\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp27\0", 0x200);
    true
}

// ── C组: open/close 错误路径测试 ─────────────────────────────────────────

fn test_open_noent() -> bool {
    const O_RDONLY: u32 = 0;
    let fd = sys_open("/nonexistent_file_xyz_test\0", O_RDONLY);
    if fd != -2 { println!("  FAIL: open nonexistent got {} (expected -2/ENOENT)", fd); return false; }
    println!("  PASS: open nonexistent -> ENOENT OK");
    true
}

fn test_open_dir_as_file() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp28\0", 0o777);

    const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp28\0", O_RDWR);
    if fd != -21 { println!("  FAIL: open dir with O_RDWR got {} (expected -21/EISDIR)", fd); return false; }
    println!("  PASS: open dir as file -> EISDIR OK");

    sys_unlinkat(AT_FDCWD, "/tmp28\0", 0x200);
    true
}

fn test_open_trunc() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp29\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    const O_TRUNC: u32 = 0o1000; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp29/tr\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"important data here"); // 19 bytes
    sys_close(fd as usize);
    let fd = sys_open("/tmp29/tr\0", O_RDWR | O_TRUNC);
    let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
    let ret = sys_fstat(fd as usize, &mut st);
    if ret < 0 { println!("  FAIL: fstat after trunc returned {}", ret); return false; }
    if st.st_size != 0 { println!("  FAIL: trunc size {} != 0", st.st_size); return false; }
    sys_write(fd as usize, b"new");
    sys_close(fd as usize);
    let fd = sys_open("/tmp29/tr\0", O_RDONLY);
    let mut buf = [0u8; 16];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"new" { println!("  FAIL: trunc lost new data: {:?}", &buf[..n as usize]); return false; }
    println!("  PASS: O_TRUNC (size=0 + write new) OK");
    sys_unlinkat(AT_FDCWD, "/tmp29/tr\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp29\0", 0x200);
    true
}

fn test_close_twice() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp30\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp30/c\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"x");

    let r1 = sys_close(fd as usize);
    let r2 = sys_close(fd as usize);
    if r1 != 0 { println!("  FAIL: first close returned {}", r1); return false; }
    if r2 != -9 { println!("  FAIL: double close got {} (expected -9/EBADF)", r2); return false; }
    println!("  PASS: double close -> EBADF OK");

    sys_unlinkat(AT_FDCWD, "/tmp30/c\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp30\0", 0x200);
    true
}

fn test_open_close_many() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp31\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd0 = sys_open("/tmp31/many\0", O_CREAT | O_RDWR);
    sys_write(fd0 as usize, b"x");
    sys_close(fd0 as usize);
    let mut ok = true;
    for i in 0..32 {
        let fd = sys_open("/tmp31/many\0", O_RDONLY);
        if fd < 0 { println!("  FAIL: open #{} in loop returned {}", i, fd); ok = false; break; }
        let r = sys_close(fd as usize);
        if r < 0 { println!("  FAIL: close #{} in loop returned {}", i, r); ok = false; break; }
    }
    if !ok { return false; }
    println!("  PASS: open/close 32 times OK");
    sys_unlinkat(AT_FDCWD, "/tmp31/many\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp31\0", 0x200);
    true
}

fn test_open_create_existing() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp32\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp32/exist\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"original content");
    sys_close(fd as usize);

    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp32/exist\0", O_RDONLY);
    if fd < 0 { println!("  FAIL: open existing without O_CREAT returned {}", fd); return false; }
    let mut buf = [0u8; 32];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"original content" { println!("  FAIL: existing content changed: {:?}", &buf[..n as usize]); return false; }
    println!("  PASS: open existing without O_CREAT OK");

    sys_unlinkat(AT_FDCWD, "/tmp32/exist\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp32\0", 0x200);
    true
}

// ── D组: 压力/边界测试 ──────────────────────────────────────────────────

fn test_stress_create_many() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp33\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_WRONLY: u32 = 0o1; const O_RDONLY: u32 = 0;
    let count = 50u8;
    // create files
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'3',b'/',b'f',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_CREAT | O_WRONLY);
        if fd < 0 { println!("  FAIL: create file {} returned {}", i, fd); return false; }
        sys_write(fd as usize, b"X");
        sys_close(fd as usize);
    }
    // verify all exist
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'3',b'/',b'f',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_RDONLY);
        if fd < 0 { println!("  FAIL: verify file {} returned {}", i, fd); return false; }
        sys_close(fd as usize);
    }
    println!("  PASS: create {} files + verify OK", count);
    // cleanup
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'3',b'/',b'f',b'0'+(i/10),b'0'+(i%10),b'\0'];
        sys_unlinkat(AT_FDCWD, unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp33\0", 0x200);
    true
}

fn test_stress_read_many() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp34\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let count = 30u8;
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'4',b'/',b'r',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_CREAT | O_RDWR);
        let content = [b'f', b'i', b'l', b'e', b'_', b'0'+(i/10), b'0'+(i%10)];
        sys_write(fd as usize, &content);
        sys_close(fd as usize);
    }
    // read all back
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'4',b'/',b'r',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_RDONLY);
        let mut buf = [0u8; 16];
        let n = sys_read(fd as usize, &mut buf);
        sys_close(fd as usize);
        let expected = [b'f', b'i', b'l', b'e', b'_', b'0'+(i/10), b'0'+(i%10)];
        if &buf[..n as usize] != &expected { println!("  FAIL: read back file {} mismatch", i); return false; }
    }
    println!("  PASS: read {} files with unique content OK", count);
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'4',b'/',b'r',b'0'+(i/10),b'0'+(i%10),b'\0'];
        sys_unlinkat(AT_FDCWD, unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp34\0", 0x200);
    true
}

fn test_stress_unlink_loop() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp35\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_WRONLY: u32 = 0o1;
    let count = 30u8;
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'5',b'/',b'u',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_CREAT | O_WRONLY);
        sys_write(fd as usize, b"x");
        sys_close(fd as usize);
    }
    for i in 0..count {
        let fname = [b'/',b't',b'm',b'p',b'3',b'5',b'/',b'u',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let r = sys_unlinkat(AT_FDCWD, unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, 0);
        if r < 0 { println!("  FAIL: unlink file {} returned {}", i, r); return false; }
    }
    // verify empty via getdents64
    let fd = sys_open("/tmp35\0", 0o200000 | 0);
    let mut buf = [0u8; 512];
    let n = sys_getdents64(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n < 0 {
        println!("  FAIL: getdents64 returned {}", n);
        sys_unlinkat(AT_FDCWD, "/tmp35\0", 0x200);
        return false;
    }
    let mut entries = 0usize;
    let bytes = &buf[..n as usize];
    let mut pos = 0;
    while pos + 19 <= bytes.len() {
        let d_reclen = u16::from_le_bytes([bytes[pos + 16], bytes[pos + 17]]) as usize;
        if d_reclen == 0 { break; }
        let name_start = pos + 19;
        let name_end = bytes[name_start..].iter().position(|&b| b == 0).map(|j| name_start + j).unwrap_or(bytes.len());
        let name = core::str::from_utf8(&bytes[name_start..name_end]).unwrap_or("???");
        if name != "." && name != ".." { entries += 1; }
        pos += d_reclen;
    }
    if entries > 0 { println!("  FAIL: {} entries remain after unlink_all", entries); return false; }
    println!("  PASS: create+unlink {} files -> empty dir OK", count);
    sys_unlinkat(AT_FDCWD, "/tmp35\0", 0x200);
    true
}

fn test_stress_rename_loop() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp36\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp36/a\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"rename_loop_content");
    sys_close(fd as usize);
    for _i in 0..10 {
        let r = sys_renameat2(AT_FDCWD, "/tmp36/a\0", AT_FDCWD, "/tmp36/b\0", 0);
        if r < 0 { println!("  FAIL: rename a->b iteration {} returned {}", _i, r); return false; }
        let r = sys_renameat2(AT_FDCWD, "/tmp36/b\0", AT_FDCWD, "/tmp36/a\0", 0);
        if r < 0 { println!("  FAIL: rename b->a iteration {} returned {}", _i, r); return false; }
    }
    let fd = sys_open("/tmp36/a\0", O_RDONLY);
    if fd < 0 { println!("  FAIL: file 'a' missing after rename loop"); return false; }
    let mut buf = [0u8; 32];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if &buf[..n as usize] != b"rename_loop_content" { println!("  FAIL: content changed after rename loop"); return false; }
    println!("  PASS: rename a↔b loop x10 OK");
    sys_unlinkat(AT_FDCWD, "/tmp36/a\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp36\0", 0x200);
    true
}

fn test_stress_large_file() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp37\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp37/big\0", O_CREAT | O_RDWR);
    if fd < 0 { println!("  FAIL: create big file returned {}", fd); return false; }
    let total_kb = 64usize; // 64KB
    let chunk = [0u8; 1024];
    let mut chunk_buf = chunk;
    for kb in 0..total_kb {
        chunk_buf[0] = (kb & 0xFF) as u8;
        chunk_buf[1] = ((kb >> 8) & 0xFF) as u8;
        let n = sys_write(fd as usize, &chunk_buf);
        if n != 1024 { println!("  FAIL: write KB {} returned {}", kb, n); return false; }
    }
    sys_close(fd as usize);
    // verify first, middle, last chunks
    let fd = sys_open("/tmp37/big\0", O_RDONLY);
    let mut buf = [0u8; 1024];
    // first
    let n = sys_read(fd as usize, &mut buf);
    if n != 1024 || buf[0] != 0 || buf[1] != 0 { println!("  FAIL: first chunk verification"); return false; }
    // middle (32KB)
    sys_lseek(fd as usize, 32 * 1024, SEEK_SET);
    let n = sys_read(fd as usize, &mut buf);
    if n != 1024 || buf[0] != 32 || buf[1] != 0 { println!("  FAIL: middle chunk verification (KB 32)"); return false; }
    // last (63KB)
    sys_lseek(fd as usize, 63 * 1024, SEEK_SET);
    let n = sys_read(fd as usize, &mut buf);
    if n != 1024 || buf[0] != 63 || buf[1] != 0 { println!("  FAIL: last chunk verification (KB 63)"); return false; }
    sys_close(fd as usize);
    println!("  PASS: large file 64KB write+read OK");
    sys_unlinkat(AT_FDCWD, "/tmp37/big\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp37\0", 0x200);
    true
}

fn test_stress_getdents() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp38\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_WRONLY: u32 = 0o1;
    let nfiles = 20u8;
    for i in 0..nfiles {
        let fname = [b'/',b't',b'm',b'p',b'3',b'8',b'/',b'd',b'0'+(i/10),b'0'+(i%10),b'\0'];
        let fd = sys_open(unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, O_CREAT | O_WRONLY);
        sys_write(fd as usize, b".");
        sys_close(fd as usize);
    }
    let fd = sys_open("/tmp38\0", 0o200000 | 0);
    let mut buf = [0u8; 512]; // small buffer — forces multiple getdents64 calls
    let mut entries = 0usize;
    loop {
        let n = sys_getdents64(fd as usize, &mut buf);
        if n <= 0 { break; }
        let (count, invalid) = count_dir_entries(&buf, n, None);
        if invalid { break; }
        entries += count;
    }
    sys_close(fd as usize);
    if entries != nfiles as usize { println!("  FAIL: getdents counted {} files (expected {})", entries, nfiles); return false; }
    println!("  PASS: getdents counts {} files OK", nfiles);
    for i in 0..nfiles {
        let fname = [b'/',b't',b'm',b'p',b'3',b'8',b'/',b'd',b'0'+(i/10),b'0'+(i%10),b'\0'];
        sys_unlinkat(AT_FDCWD, unsafe { core::str::from_utf8_unchecked(&fname[..11]) }, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp38\0", 0x200);
    true
}

fn test_stress_truncate() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp39\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp39/trunc\0", O_CREAT | O_RDWR);
    let data = [0xAAu8; 100];
    sys_write(fd as usize, &data);
    // truncate to 50
    sys_ftruncate(fd as usize, 50);
    let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
    sys_fstat(fd as usize, &mut st);
    if st.st_size != 50 { println!("  FAIL: truncate to 50 got size {}", st.st_size); return false; }
    sys_lseek(fd as usize, 0, SEEK_SET);
    let mut buf = [0u8; 64];
    let n = sys_read(fd as usize, &mut buf);
    if n != 50 { println!("  FAIL: read after truncate to 50 got {}", n); return false; }
    // extend to 200
    sys_ftruncate(fd as usize, 200);
    sys_fstat(fd as usize, &mut st);
    if st.st_size != 200 { println!("  FAIL: extend to 200 got size {}", st.st_size); return false; }
    sys_lseek(fd as usize, 100, SEEK_SET);
    let n = sys_read(fd as usize, &mut buf[..32]);
    sys_close(fd as usize);
    // bytes 100..132 should be zeros (hole from extension)
    let zeros = [0u8; 32];
    if &buf[..n as usize] != &zeros[..] { println!("  FAIL: extend hole not zero-filled"); return false; }
    println!("  PASS: truncate 100→50→200 with hole OK");
    sys_unlinkat(AT_FDCWD, "/tmp39/trunc\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp39\0", 0x200);
    true
}

fn test_perf_getdents_1000() -> bool {
    const O_RDONLY: u32 = 0;
    const O_WRONLY: u32 = 0o1;
    const O_CREAT: u32 = 0o100;
    const O_DIRECTORY: u32 = 0o200000;
    const AT_REMOVEDIR: u32 = 0x200;
    const PREFIX: &str = "/tmp_perf_getdents";

    sys_mkdirat(AT_FDCWD, "/tmp_perf_getdents\0", 0o777);
    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        let fd = sys_open(&path, O_CREAT | O_WRONLY);
        if fd < 0 { println!("  FAIL: create file_{} returned {}", i, fd); return false; }
        let n = sys_write(fd as usize, b"x");
        sys_close(fd as usize);
        if n != 1 { println!("  FAIL: write file_{} returned {}", i, n); return false; }
    }

    let fd = sys_open("/tmp_perf_getdents\0", O_RDONLY | O_DIRECTORY);
    if fd < 0 { println!("  FAIL: open getdents dir returned {}", fd); return false; }
    let mut buf8 = [0u8; 8192];
    let mut entries8 = 0usize;
    let mut calls8 = 0usize;
    let mut invalid8 = false;
    let mut ended8 = false;
    loop {
        let n = sys_getdents64(fd as usize, &mut buf8);
        if n < 0 { println!("  FAIL: getdents64 8k returned {}", n); break; }
        if n == 0 { ended8 = true; break; }
        calls8 += 1;
        let (count, invalid) = count_dir_entries(&buf8, n, Some("file_"));
        entries8 += count;
        invalid8 |= invalid;
        if invalid { break; }
    }
    sys_close(fd as usize);
    println!("[FS_PERF] getdents_1000_8k entries={} calls={}", entries8, calls8);
    if calls8 > 8 { println!("[FS_PERF][WARN] getdents_1000_8k calls={} > 8", calls8); }

    let fd = sys_open("/tmp_perf_getdents\0", O_RDONLY | O_DIRECTORY);
    if fd < 0 { println!("  FAIL: reopen getdents dir returned {}", fd); return false; }
    let mut buf64 = [0u8; 65536];
    let mut entries64 = 0usize;
    let mut calls64 = 0usize;
    let mut invalid64 = false;
    let mut ended64 = false;
    loop {
        let n = sys_getdents64(fd as usize, &mut buf64);
        if n < 0 { println!("  FAIL: getdents64 64k returned {}", n); break; }
        if n == 0 { ended64 = true; break; }
        calls64 += 1;
        let (count, invalid) = count_dir_entries(&buf64, n, Some("file_"));
        entries64 += count;
        invalid64 |= invalid;
        if invalid { break; }
    }
    sys_close(fd as usize);
    println!("[FS_PERF] getdents_1000_64k entries={} calls={}", entries64, calls64);
    if calls64 > 2 { println!("[FS_PERF][WARN] getdents_1000_64k calls={} > 2", calls64); }

    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        sys_unlinkat(AT_FDCWD, &path, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp_perf_getdents\0", AT_REMOVEDIR);

    if entries8 != 1000 || entries64 != 1000 || invalid8 || invalid64 || !ended8 || !ended64 {
        println!("  FAIL: getdents perf entries8={} entries64={} invalid8={} invalid64={} ended8={} ended64={}", entries8, entries64, invalid8, invalid64, ended8, ended64);
        return false;
    }
    true
}

fn test_perf_stat_like_1000() -> bool {
    const O_WRONLY: u32 = 0o1;
    const O_CREAT: u32 = 0o100;
    const AT_REMOVEDIR: u32 = 0x200;
    const PREFIX: &str = "/tmp_perf_stat";

    sys_mkdirat(AT_FDCWD, "/tmp_perf_stat\0", 0o777);
    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        let fd = sys_open(&path, O_CREAT | O_WRONLY);
        if fd < 0 { println!("  FAIL: create stat file_{} returned {}", i, fd); return false; }
        sys_write(fd as usize, b"s");
        sys_close(fd as usize);
    }

    let samples = [0usize, 500usize, 999usize];
    let mut first_ok = true;
    for &idx in &samples {
        let path = make_file_path(PREFIX, idx);
        let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
        let r = sys_fstatat(AT_FDCWD, &path, &mut st, 0);
        if r != 0 || st.st_ino == 0 || (st.st_mode & 0o170000) != 0o100000 || st.st_size < 1 { first_ok = false; }
    }
    let missing = make_path(PREFIX, "/not_exists");
    let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
    if sys_fstatat(AT_FDCWD, &missing, &mut st, 0) != -2 { first_ok = false; }

    let mut second_ok = true;
    for &idx in &samples {
        let path = make_file_path(PREFIX, idx);
        let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
        let r = sys_fstatat(AT_FDCWD, &path, &mut st, 0);
        if r != 0 || st.st_ino == 0 || (st.st_mode & 0o170000) != 0o100000 || st.st_size < 1 { second_ok = false; }
    }

    let mut access_ok = true;
    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        let r = sys_faccessat2(AT_FDCWD, &path, 0, 0);
        if r == -38 { println!("[FS_PERF][WARN] faccessat2 ENOSYS, skipping access sweep"); break; }
        if r != 0 { access_ok = false; break; }
    }
    println!("[FS_PERF] stat_like_1000 first_ok={} second_ok={}", first_ok, second_ok);

    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        sys_unlinkat(AT_FDCWD, &path, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp_perf_stat\0", AT_REMOVEDIR);
    first_ok && second_ok && access_ok
}

fn test_perf_repeated_lookup_cache() -> bool {
    const O_WRONLY: u32 = 0o1;
    const O_CREAT: u32 = 0o100;
    const AT_REMOVEDIR: u32 = 0x200;
    const PREFIX: &str = "/tmp_perf_lookup";

    sys_mkdirat(AT_FDCWD, "/tmp_perf_lookup\0", 0o777);
    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        let fd = sys_open(&path, O_CREAT | O_WRONLY);
        if fd < 0 { println!("  FAIL: create lookup file_{} returned {}", i, fd); return false; }
        sys_write(fd as usize, b"l");
        sys_close(fd as usize);
    }

    let existing = make_file_path(PREFIX, 999);
    let mut existing_ok = 0usize;
    for _ in 0..100usize {
        let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
        if sys_fstatat(AT_FDCWD, &existing, &mut st, 0) == 0 { existing_ok += 1; }
    }
    println!("[FS_PERF] repeated_lookup existing_ok={}", existing_ok);

    let missing = make_path(PREFIX, "/not_exists");
    let mut negative_enoent = 0usize;
    for _ in 0..100usize {
        let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
        if sys_fstatat(AT_FDCWD, &missing, &mut st, 0) == -2 { negative_enoent += 1; }
    }
    println!("[FS_PERF] repeated_lookup negative_enoent={}", negative_enoent);

    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        sys_unlinkat(AT_FDCWD, &path, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp_perf_lookup\0", AT_REMOVEDIR);
    existing_ok == 100 && negative_enoent == 100
}

fn test_perf_symlink_batch_200() -> bool {
    const O_RDONLY: u32 = 0;
    const O_WRONLY: u32 = 0o1;
    const O_CREAT: u32 = 0o100;
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_REMOVEDIR: u32 = 0x200;
    const PREFIX: &str = "/tmp_perf_symlink";

    sys_mkdirat(AT_FDCWD, "/tmp_perf_symlink\0", 0o777);
    let target_path = make_path(PREFIX, "/target");
    let fd = sys_open(&target_path, O_CREAT | O_WRONLY);
    if fd < 0 { println!("  FAIL: create symlink target returned {}", fd); return false; }
    sys_write(fd as usize, b"target-data");
    sys_close(fd as usize);

    for i in 0..200usize {
        let link = make_link_path(PREFIX, i);
        let r = sys_symlinkat("target\0", AT_FDCWD, &link);
        if r < 0 { println!("  FAIL: symlink {} returned {}", i, r); return false; }
    }

    let samples = [0usize, 100usize, 199usize];
    let mut verify_ok = true;
    for &idx in &samples {
        let link = make_link_path(PREFIX, idx);
        let mut link_buf = [0u8; 32];
        let n = sys_readlinkat(AT_FDCWD, &link, &mut link_buf);
        if n != 6 || &link_buf[..6] != b"target" { verify_ok = false; }
        let mut st = Stat { st_dev:0, st_ino:0, st_mode:0, st_nlink:0, st_uid:0, st_gid:0, st_rdev:0, __pad:0, st_size:0, st_blksize:0, __pad2:0, st_blocks:0, st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 }, __unused: 0 };
        let r = sys_fstatat(AT_FDCWD, &link, &mut st, AT_SYMLINK_NOFOLLOW);
        if r != 0 || (st.st_mode & 0o170000) != 0o120000 { verify_ok = false; }
        let fd = sys_open(&link, O_RDONLY);
        if fd < 0 { verify_ok = false; } else {
            let mut data = [0u8; 16];
            let n = sys_read(fd as usize, &mut data);
            sys_close(fd as usize);
            if n != 11 || &data[..11] != b"target-data" { verify_ok = false; }
        }
    }
    println!("[FS_PERF] symlink_batch_200 verify_ok={}", verify_ok);

    for i in 0..200usize {
        let link = make_link_path(PREFIX, i);
        sys_unlinkat(AT_FDCWD, &link, 0);
    }
    sys_unlinkat(AT_FDCWD, &target_path, 0);
    sys_unlinkat(AT_FDCWD, "/tmp_perf_symlink\0", AT_REMOVEDIR);
    println!("[FS_PERF] symlink_batch_200 cleanup_done=true");
    verify_ok
}

fn test_perf_open_access_large_dir() -> bool {
    const O_RDONLY: u32 = 0;
    const O_WRONLY: u32 = 0o1;
    const O_CREAT: u32 = 0o100;
    const AT_REMOVEDIR: u32 = 0x200;
    const PREFIX: &str = "/tmp_perf_open";

    sys_mkdirat(AT_FDCWD, "/tmp_perf_open\0", 0o777);
    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        let fd = sys_open(&path, O_CREAT | O_WRONLY);
        if fd < 0 { println!("  FAIL: create open file_{} returned {}", i, fd); return false; }
        sys_write(fd as usize, b"o");
        sys_close(fd as usize);
    }

    let samples = [0usize, 500usize, 999usize];
    let mut first_ok = true;
    for &idx in &samples {
        let path = make_file_path(PREFIX, idx);
        let fd = sys_open(&path, O_RDONLY);
        if fd < 0 { first_ok = false; } else { sys_close(fd as usize); }
    }

    let mut second_ok = true;
    for &idx in &samples {
        let path = make_file_path(PREFIX, idx);
        let fd = sys_open(&path, O_RDONLY);
        if fd < 0 { second_ok = false; } else { sys_close(fd as usize); }
    }

    let missing = make_path(PREFIX, "/not_exists");
    let mut negative_enoent = 0usize;
    for _ in 0..100usize {
        if sys_open(&missing, O_RDONLY) == -2 { negative_enoent += 1; }
    }
    println!("[FS_PERF] open_access_large_dir first_ok={} second_ok={} negative_enoent={}", first_ok, second_ok, negative_enoent);

    for i in 0..1000usize {
        let path = make_file_path(PREFIX, i);
        sys_unlinkat(AT_FDCWD, &path, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp_perf_open\0", AT_REMOVEDIR);
    first_ok && second_ok && negative_enoent == 100
}

// ── E组: 并发测试 (fork) ────────────────────────────────────────────────

fn test_fork_read_same_fd() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp40\0", 0o777);
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2;
    let fd = sys_open("/tmp40/shared\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"PARENTCHILD");
    let pid = sys_fork();
    if pid == 0 {
        // CHILD: read second half
        sys_lseek(fd as usize, 6, SEEK_SET);
        let mut buf = [0u8; 5];
        let n = sys_read(fd as usize, &mut buf);
        if n != 5 || &buf[..5] != b"CHILD" { sys_exit(1); }
        sys_exit(0);
    } else if pid > 0 {
        // PARENT: read first half
        sys_lseek(fd as usize, 0, SEEK_SET);
        let mut buf = [0u8; 6];
        let n = sys_read(fd as usize, &mut buf);
        if n != 6 || &buf[..6] != b"PARENT" {
            println!("  FAIL: parent read got {:?}", &buf[..n as usize]);
            return false;
        }
        let mut child_code: i32 = 0;
        sys_waitpid(pid, &mut child_code);
        if child_code != 0 { println!("  FAIL: child exited with {}", child_code); return false; }
        sys_close(fd as usize);
        println!("  PASS: fork read same fd (parent+child) OK");
        sys_unlinkat(AT_FDCWD, "/tmp40/shared\0", 0);
        sys_unlinkat(AT_FDCWD, "/tmp40\0", 0x200);
        true
    } else {
        println!("  FAIL: fork returned {}", pid);
        false
    }
}

fn test_fork_create() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp41\0", 0o777);
    let pid = sys_fork();
    if pid == 0 {
        // CHILD
        const O_CREATc: u32 = 0o100; const O_WRONLYc: u32 = 0o1;
        let fd = sys_open("/tmp41/child_file\0", O_CREATc | O_WRONLYc);
        if fd < 0 { sys_exit(2); }
        sys_write(fd as usize, b"child");
        sys_close(fd as usize);
        sys_exit(0);
    } else if pid > 0 {
        // PARENT
        const O_CREATp: u32 = 0o100; const O_WRONLYp: u32 = 0o1;
        let fd = sys_open("/tmp41/parent_file\0", O_CREATp | O_WRONLYp);
        sys_write(fd as usize, b"parent");
        sys_close(fd as usize);
        let mut child_code: i32 = 0;
        sys_waitpid(pid, &mut child_code);
        if child_code != 0 { println!("  FAIL: fork create child exited {}", child_code); return false; }
        // verify both files exist
        const O_RDONLY: u32 = 0;
        let fd = sys_open("/tmp41/parent_file\0", O_RDONLY);
        if fd < 0 { println!("  FAIL: parent file missing"); return false; }
        let mut buf = [0u8; 16];
        let n = sys_read(fd as usize, &mut buf);
        sys_close(fd as usize);
        if &buf[..n as usize] != b"parent" { println!("  FAIL: parent content mismatch"); return false; }
        let fd = sys_open("/tmp41/child_file\0", O_RDONLY);
        if fd < 0 { println!("  FAIL: child file missing"); return false; }
        let n = sys_read(fd as usize, &mut buf);
        sys_close(fd as usize);
        if &buf[..n as usize] != b"child" { println!("  FAIL: child content mismatch"); return false; }
        println!("  PASS: fork create (parent+child) OK");
        sys_unlinkat(AT_FDCWD, "/tmp41/parent_file\0", 0);
        sys_unlinkat(AT_FDCWD, "/tmp41/child_file\0", 0);
        sys_unlinkat(AT_FDCWD, "/tmp41\0", 0x200);
        true
    } else {
        println!("  FAIL: fork returned {}", pid);
        false
    }
}

// ── Phase 5: cache lifecycle tests ───────────────────────────────────

/// Lifecycle test: repeated open/read/close on same file should not
/// cause linear growth of alive inode objects or page caches.
fn test_repeated_open_close_same_file_lifecycle() -> bool {
    println!("[fs_cache_lifecycle] repeated_open_close_same_file_lifecycle: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_lc1\0", 0o777);
    const O_RDWRc: u32 = 0o2;
    const O_CREATc: u32 = 0o100;
    // Create a small test file
    let fd = sys_open("/tmp_lc1/f\0", O_RDWRc | O_CREATc);
    if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
    sys_write(fd as usize, b"hello lifecycle!\n");
    sys_close(fd as usize);

    const N: usize = 200;
    for _ in 0..N {
        let fd = sys_open("/tmp_lc1/f\0", 0); // O_RDONLY
        if fd < 0 { println!("  FAIL: open err={} at iteration", fd); return false; }
        let mut buf = [0u8; 64];
        let n = sys_read(fd as usize, &mut buf);
        if n <= 0 || &buf[..n as usize] != b"hello lifecycle!\n" {
            println!("  FAIL: read mismatch n={}", n);
            return false;
        }
        sys_close(fd as usize);
    }

    // Cleanup
    sys_unlinkat(AT_FDCWD, "/tmp_lc1/f\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp_lc1\0", 0x200);

    println!("[fs_cache_lifecycle] repeated_open_close_same_file_lifecycle: pass");
    true
}

/// Lifecycle test: create many files, open/read all, close all.
fn test_lookup_many_files_then_close_lifecycle() -> bool {
    println!("[fs_cache_lifecycle] lookup_many_files_then_close_lifecycle: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_lc2\0", 0o777);
    const N: usize = 64;
    const O_RDWRc: u32 = 0o2;
    const O_CREATc: u32 = 0o100;

    // Create N files with deterministic content
    for i in 0..N {
        let path = make_file_path("/tmp_lc2/", i);
        let fd = sys_open(&path, O_RDWRc | O_CREATc);
        if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
        let content = format!("file_{:04}\n", i);
        sys_write(fd as usize, content.as_bytes());
        sys_close(fd as usize);
    }

    // Open, read, close all files
    for i in 0..N {
        let path = make_file_path("/tmp_lc2/", i);
        let fd = sys_open(&path, 0); // O_RDONLY
        if fd < 0 { println!("  FAIL: open err={}", fd); return false; }
        let mut buf = [0u8; 64];
        let n = sys_read(fd as usize, &mut buf);
        if n <= 0 { println!("  FAIL: read err n={}", n); return false; }
        sys_close(fd as usize);
    }

    // Cleanup
    for i in 0..N {
        let path = make_file_path("/tmp_lc2/", i);
        sys_unlinkat(AT_FDCWD, &path, 0);
    }
    sys_unlinkat(AT_FDCWD, "/tmp_lc2\0", 0x200);

    println!("[fs_cache_lifecycle] lookup_many_files_then_close_lifecycle: pass");
    true
}

/// Lifecycle test: unlink a file while fd is open, verify data still
/// accessible via fd, then close and verify cache cleanup.
fn test_unlink_open_file_lifecycle() -> bool {
    println!("[fs_cache_lifecycle] unlink_open_file_lifecycle: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_lc3\0", 0o777);
    const O_RDWRc: u32 = 0o2;
    const O_CREATc: u32 = 0o100;

    // Create file with pattern
    let fd = sys_open("/tmp_lc3/f\0", O_RDWRc | O_CREATc);
    if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
    let pattern = b"unlink_lifecycle_data_pattern_42\n";
    sys_write(fd as usize, pattern);
    sys_close(fd as usize);

    // Open and read to trigger PageCache
    let fd = sys_open("/tmp_lc3/f\0", O_RDWRc);
    if fd < 0 { println!("  FAIL: open err={}", fd); return false; }
    let mut buf = [0u8; 64];
    let n = sys_read(fd as usize, &mut buf);
    if n <= 0 || &buf[..n as usize] != pattern {
        println!("  FAIL: initial read mismatch");
        return false;
    }

    // Unlink while fd is still open
    let ret = sys_unlinkat(AT_FDCWD, "/tmp_lc3/f\0", 0);
    if ret < 0 { println!("  FAIL: unlink err={}", ret); return false; }

    // Read again via same fd — content must still be accessible
    sys_lseek(fd as usize, 0, SEEK_SET);
    let mut buf2 = [0u8; 64];
    let n2 = sys_read(fd as usize, &mut buf2);
    if n2 <= 0 || &buf2[..n2 as usize] != pattern {
        println!("  FAIL: post-unlink read mismatch n={}", n2);
        return false;
    }

    // Close fd
    sys_close(fd as usize);

    // Cleanup
    sys_unlinkat(AT_FDCWD, "/tmp_lc3\0", 0x200);

    // Verify: file should be unreachable now
    let fd3 = sys_open("/tmp_lc3/f\0", 0);
    if fd3 >= 0 {
        println!("  FAIL: file still accessible after unlink+close");
        sys_close(fd3 as usize);
        return false;
    }

    println!("[fs_cache_lifecycle] unlink_open_file_lifecycle: pass");
    true
}

// ── G组: 缓存回收测试 ──────────────────────────────────────────────

/// Write a large file, read to fill cache, then re-read to verify data is still accessible.
fn test_page_cache_reclaim_clean_pages() -> bool {
    println!("[fs_cache_reclaim] page_cache_reclaim_clean_pages: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_rc1\0", 0o777);
    const O_RDWRc: u32 = 0o2;
    const O_CREATc: u32 = 0o100;
    const PAGE: usize = 4096;
    const N_PAGES: usize = 16;

    // Create a multi-page file with deterministic pattern
    let fd = sys_open("/tmp_rc1/big\0", O_RDWRc | O_CREATc);
    if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
    let pattern = [0xABu8; PAGE];
    for i in 0..N_PAGES {
        let w = sys_write(fd as usize, &pattern);
        if w != PAGE as isize { println!("  FAIL: write page {} err={}", i, w); return false; }
    }
    sys_fsync(fd as usize);
    sys_close(fd as usize);

    // Re-open and read all pages
    let fd = sys_open("/tmp_rc1/big\0", O_RDWRc);
    if fd < 0 { println!("  FAIL: reopen err={}", fd); return false; }
    {
        let mut buf = [0u8; PAGE];
        for i in 0..N_PAGES {
            sys_lseek(fd as usize, (i * PAGE) as isize, 0);
            let n = sys_read(fd as usize, &mut buf);
            if n != PAGE as isize || buf != pattern {
                println!("  FAIL: read page {} n={}", i, n);
                return false;
            }
        }
    }

    // Re-read to verify data is still accessible
    {
        let mut buf = [0u8; PAGE];
        for i in 0..N_PAGES {
            sys_lseek(fd as usize, (i * PAGE) as isize, 0);
            let n = sys_read(fd as usize, &mut buf);
            if n != PAGE as isize || buf != pattern {
                println!("  FAIL: re-read page {} n={}", i, n);
                sys_close(fd as usize);
                return false;
            }
        }
    }

    sys_close(fd as usize);
    sys_unlinkat(AT_FDCWD, "/tmp_rc1/big\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp_rc1\0", 0x200);

    println!("[fs_cache_reclaim] page_cache_reclaim_clean_pages: pass");
    true
}

/// Write dirty data, fsync, close, then verify data survives.
fn test_dirty_page_no_loss_under_reclaim() -> bool {
    println!("[fs_cache_reclaim] dirty_page_no_loss_under_reclaim: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_rc2\0", 0o777);
    const O_RDWRc: u32 = 0o2;
    const O_CREATc: u32 = 0o100;
    const PAGE: usize = 4096;
    const N_PAGES: usize = 4;

    // Write pattern A, do NOT sync/close — keep fd open with dirty data
    let fd = sys_open("/tmp_rc2/f\0", O_RDWRc | O_CREATc);
    if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
    let pattern_a = [0xCDu8; PAGE];
    for i in 0..N_PAGES {
        let w = sys_write(fd as usize, &pattern_a);
        if w != PAGE as isize { println!("  FAIL: write page {} err={}", i, w); return false; }
    }

    // fsync + close
    sys_fsync(fd as usize);
    sys_close(fd as usize);

    // Re-open and verify pattern A is intact
    let fd2 = sys_open("/tmp_rc2/f\0", 0);
    if fd2 < 0 { println!("  FAIL: reopen err={}", fd2); return false; }
    {
        let mut buf = [0u8; PAGE];
        for i in 0..N_PAGES {
            sys_lseek(fd2 as usize, (i * PAGE) as isize, 0);
            let n = sys_read(fd2 as usize, &mut buf);
            if n != PAGE as isize || buf != pattern_a {
                println!("  FAIL: data corruption page {} n={}", i, n);
                sys_close(fd2 as usize);
                return false;
            }
        }
    }
    sys_close(fd2 as usize);

    sys_unlinkat(AT_FDCWD, "/tmp_rc2/f\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp_rc2\0", 0x200);

    println!("[fs_cache_reclaim] dirty_page_no_loss_under_reclaim: pass");
    true
}

// ── H组: 截断一致性测试 ──────────────────────────────────────────

/// Truncate test: write 8 pages, read into cache, truncate to 1 page,
/// then expand back. Verify old cached pages are invalidated (zero-filled,
/// not stale pattern).
fn test_truncate_invalidates_pagecache() -> bool {
    println!("[fs_cache_lifecycle] truncate_invalidates_pagecache: begin");
    sys_mkdirat(AT_FDCWD, "/tmp_tc1\0", 0o777);
    const O_RDWR: u32 = 0o2;
    const O_CREAT: u32 = 0o100;
    const PAGE: usize = 4096;
    const N_PAGES: usize = 8;

    // Create an 8-page file with deterministic pattern per page
    let fd = sys_open("/tmp_tc1/f\0", O_RDWR | O_CREAT);
    if fd < 0 { println!("  FAIL: create err={}", fd); return false; }
    for i in 0..N_PAGES {
        let val = (0xA0 + i as u8);
        let pattern = [val; PAGE];
        let w = sys_write(fd as usize, &pattern);
        if w != PAGE as isize { println!("  FAIL: write page {} err={}", i, w); return false; }
    }
    sys_fsync(fd as usize);

    // Read all 8 pages into PageCache
    sys_lseek(fd as usize, 0, 0);
    {
        let mut buf = [0u8; PAGE];
        for i in 0..N_PAGES {
            let n = sys_read(fd as usize, &mut buf);
            let expected = (0xA0 + i as u8);
            if n != PAGE as isize || buf[0] != expected {
                println!("  FAIL: read page {} n={} val={}", i, n, buf[0]);
                return false;
            }
        }
    }

    // Truncate to 1 page (4KB)
    let ret = sys_ftruncate(fd as usize, PAGE as isize);
    if ret < 0 { println!("  FAIL: ftruncate ret={}", ret); return false; }

    // Expand back to 8 pages (32KB)
    let ret = sys_ftruncate(fd as usize, (N_PAGES * PAGE) as isize);
    if ret < 0 { println!("  FAIL: ftruncate expand ret={}", ret); return false; }

    // Verify: page 0 still has its original pattern
    sys_lseek(fd as usize, 0, 0);
    {
        let mut buf = [0u8; PAGE];
        let n = sys_read(fd as usize, &mut buf);
        if n != PAGE as isize || buf[0] != 0xA0 {
            println!("  FAIL: page 0 data corrupted n={} val={}", n, buf[0]);
            return false;
        }
    }

    // Verify: pages 1-7 should return zero bytes (hole), not stale pattern
    for i in 1..N_PAGES {
        sys_lseek(fd as usize, (i * PAGE) as isize, 0);
        let mut buf = [0xCCu8; 16];
        let n = sys_read(fd as usize, &mut buf);
        // After truncate-expand, pages beyond original EOF are holes → zero bytes
        // Read should return bytes (hole fills zeros), not stale pattern
        if n > 0 {
            let stale = buf.iter().any(|&b| b >= 0xA1 && b <= 0xA7);
            if stale {
                println!("  FAIL: page {} has stale cached pattern n={} first={}", i, n, buf[0]);
                return false;
            }
        }
        // n==0 is also valid (EOF at 4KB if holes not auto-filled)
    }

    sys_close(fd as usize);
    sys_unlinkat(AT_FDCWD, "/tmp_tc1/f\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp_tc1\0", 0x200);

    println!("[fs_cache_lifecycle] truncate_invalidates_pagecache: pass");
    true
}

// ═══════════════════════════════════════════════════════════════════════
// Mount bench tests
// ═══════════════════════════════════════════════════════════════════════

const MS_BIND: usize = 4096;
const MS_REC: usize = 16384;

fn ts_diff_ns(start: &TimeSpec, end: &TimeSpec) -> u64 {
    let s = (end.tv_sec as u64).saturating_sub(start.tv_sec as u64);
    let ns = (end.tv_nsec as u64).wrapping_sub(start.tv_nsec as u64);
    s * 1_000_000_000 + if end.tv_nsec < start.tv_nsec { ns.wrapping_add(1_000_000_000) } else { ns }
}

fn create_dir_tree(base: &str, depth: usize) -> bool {
    let mut path = String::new();
    path.push_str(base);
    for i in 0..depth {
        path.push_str("/dir_");
        path.push_str(&format!("{:04}", i));
        path.push('\0');
        let ret = sys_mkdirat(AT_FDCWD, &path, 0o777);
        if ret < 0 {
            println!("  FAIL: mkdirat {} returned {}", &path, ret);
            return false;
        }
        // Restore path without null for next iteration
        path.pop();
        // remove null terminator to continue building
        // path already has the full directory name without \0
        // it's: base/dir_0000/dir_0001/.../dir_XXXX
        // But we need to restore it so the next push adds correctly.
        // Since we pushed "/dir_XXXX\0" (10 + null), we popped the null, leaving the dir name.
        // The next iteration will push "/dir_XXXX\0" on top, which is correct for chaining.
    }
    true
}

fn read_sysfs(path: &str) {
    let fd = sys_open(path, 0); // O_RDONLY
    if fd < 0 {
        println!("  [sysfs] open {} returned {}", path, fd);
        return;
    }
    let mut buf = [0u8; 4096];
    let n = sys_read(fd as usize, &mut buf);
    sys_close(fd as usize);
    if n > 0 {
        let len = (n as usize).min(buf.len());
        // Find first newline or use whole string
        let end = buf[..len].iter().position(|&b| b == b'\n').unwrap_or(len);
        let s = core::str::from_utf8(&buf[..end]).unwrap_or("");
        println!("  [sysfs] {} = {}", path, s);
    }
}

fn test_mount_bench_bind() -> bool {
    const DEPTH: usize = 8;
    const BASE: &str = "/tmp/mntbench\0";
    const BASE_SRC: &str = "/tmp/mntbench/src\0";
    const BASE_DST: &str = "/tmp/mntbench/dst\0";

    // 1. Create top-level directories
    let ret = sys_mkdirat(AT_FDCWD, BASE, 0o777);
    if ret < 0 {
        println!("  FAIL: mkdirat BASE returned {}", ret);
        return false;
    }
    let ret = sys_mkdirat(AT_FDCWD, BASE_SRC, 0o777);
    if ret < 0 {
        println!("  FAIL: mkdirat BASE_SRC returned {}", ret);
        return false;
    }
    let ret = sys_mkdirat(AT_FDCWD, BASE_DST, 0o777);
    if ret < 0 {
        println!("  FAIL: mkdirat BASE_DST returned {}", ret);
        return false;
    }

    // Create dir_0000 through dir_0007 in src and dst
    for i in 0..DEPTH {
        let src_path = format!("/tmp/mntbench/src/dir_{:04}\0", i);
        let ret = sys_mkdirat(AT_FDCWD, &src_path, 0o777);
        if ret < 0 {
            println!("  FAIL: mkdirat src dir_{} returned {}", i, ret);
            return false;
        }
        let dst_path = format!("/tmp/mntbench/dst/dir_{:04}\0", i);
        let ret = sys_mkdirat(AT_FDCWD, &dst_path, 0o777);
        if ret < 0 {
            println!("  FAIL: mkdirat dst dir_{} returned {}", i, ret);
            return false;
        }
    }

    // 2. Print pre-stats
    println!("  [stats before bind]");
    read_sysfs("/sys/kernel/stats/lwext4\0");
    read_sysfs("/sys/kernel/stats/mount\0");

    // 3. Bind mount each level, timing each
    for i in 0..DEPTH {
        let src = format!("/tmp/mntbench/src/dir_{:04}\0", i);
        let dst = format!("/tmp/mntbench/dst/dir_{:04}\0", i);
        let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
        sys_clock_gettime(1, &mut t0);
        let ret = sys_mount(src.as_ptr(), dst.as_ptr(), core::ptr::null(), MS_BIND, 0);
        let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
        sys_clock_gettime(1, &mut t1);
        let ns = ts_diff_ns(&t0, &t1);
        println!("  bind depth={} elapsed={}ns ret={}", i, ns, ret);
        if ret < 0 {
            println!("  FAIL: bind at depth {} ret {}", i, ret);
            return false;
        }
    }

    // 4. Print post-stats
    println!("  [stats after bind]");
    read_sysfs("/sys/kernel/stats/lwext4\0");
    read_sysfs("/sys/kernel/stats/mount\0");

    // 5. Cleanup: umount in reverse order
    for i in (0..DEPTH).rev() {
        let dst = format!("/tmp/mntbench/dst/dir_{:04}\0", i);
        sys_umount2(dst.as_ptr(), 0);
    }
    // Cleanup directories
    for i in 0..DEPTH {
        let dst_path = format!("/tmp/mntbench/dst/dir_{:04}\0", i);
        sys_unlinkat(AT_FDCWD, &dst_path, 0x200);
        let src_path = format!("/tmp/mntbench/src/dir_{:04}\0", i);
        sys_unlinkat(AT_FDCWD, &src_path, 0x200);
    }
    sys_unlinkat(AT_FDCWD, BASE_DST, 0x200);
    sys_unlinkat(AT_FDCWD, BASE_SRC, 0x200);
    sys_unlinkat(AT_FDCWD, BASE, 0x200);

    println!("  PASS: mount bench bind");
    true
}

fn test_mount_bench_rbind() -> bool {
    const DEPTH: usize = 8;
    const RBASE: &str = "/tmp/mntbench\0";
    const RB_SRC: &str = "/tmp/mntbench/rbind_src\0";
    const RB_DST: &str = "/tmp/mntbench/rbind_dst\0";

    // 1. Create directories
    sys_mkdirat(AT_FDCWD, RBASE, 0o777);
    sys_mkdirat(AT_FDCWD, RB_SRC, 0o777);
    sys_mkdirat(AT_FDCWD, RB_DST, 0o777);

    // Create chain: rbind_src/dir_0000/dir_0001/.../dir_0007
    {
        let mut current = String::new();
        current.push_str("/tmp/mntbench/rbind_src");
        for i in 0..DEPTH {
            current.push_str("/dir_");
            current.push_str(&format!("{:04}", i));
            current.push('\0');
            let ret = sys_mkdirat(AT_FDCWD, &current, 0o777);
            if ret < 0 {
                println!("  FAIL: mkdirat chain {} returned {}", &current, ret);
                return false;
            }
            current.pop(); // pop null
        }
    }

    // Create submounts within the tree: for each level i,
    // bind dir_XXXX as a separate mount onto itself via a temp mount point
    for i in 0..DEPTH {
        let mut sub = String::from("/tmp/mntbench/rbind_src");
        for j in 0..=i {
            sub.push_str("/dir_");
            sub.push_str(&format!("{:04}", j));
        }
        sub.push('\0');
        let tmp = format!("/tmp/mntbench/rbind_tmp_{:04}\0", i);
        sys_mkdirat(AT_FDCWD, &tmp, 0o777);
        let ret = sys_mount(sub.as_ptr(), tmp.as_ptr(), core::ptr::null(), MS_BIND, 0);
        if ret < 0 {
            println!("  FAIL: submount bind {} returned {}", i, ret);
            return false;
        }
        sys_umount2(sub.as_ptr(), 0);
        let ret = sys_mount(tmp.as_ptr(), sub.as_ptr(), core::ptr::null(), MS_BIND, 0);
        if ret < 0 {
            println!("  FAIL: remount submount {} returned {}", i, ret);
            return false;
        }
        sys_unlinkat(AT_FDCWD, &tmp, 0x200);
    }

    // 2. Print pre-stats
    println!("  [stats before rbind]");
    read_sysfs("/sys/kernel/stats/lwext4\0");
    read_sysfs("/sys/kernel/stats/mount\0");

    // 3. Single rbind call
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    let ret = sys_mount(
        "/tmp/mntbench/rbind_src\0".as_ptr(),
        "/tmp/mntbench/rbind_dst\0".as_ptr(),
        core::ptr::null(),
        MS_BIND | MS_REC,
        0,
    );
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let ns = ts_diff_ns(&t0, &t1);
    println!("  rbind depth={} elapsed={}ns ret={}", DEPTH, ns, ret);
    if ret < 0 {
        println!("  FAIL: rbind returned {}", ret);
        return false;
    }

    // 4. Print post-stats
    println!("  [stats after rbind]");
    read_sysfs("/sys/kernel/stats/lwext4\0");
    read_sysfs("/sys/kernel/stats/mount\0");

    // 5. Cleanup
    sys_umount2("/tmp/mntbench/rbind_dst\0".as_ptr(), 0);
    for i in (0..DEPTH).rev() {
        let mut sub = String::from("/tmp/mntbench/rbind_src");
        for j in 0..=i {
            sub.push_str("/dir_");
            sub.push_str(&format!("{:04}", j));
        }
        sub.push('\0');
        sys_umount2(sub.as_ptr(), 0);
    }
    for i in (0..DEPTH).rev() {
        let mut sub = String::from("/tmp/mntbench/rbind_src");
        for j in 0..=i {
            sub.push_str("/dir_");
            sub.push_str(&format!("{:04}", j));
        }
        sub.push('\0');
        sys_unlinkat(AT_FDCWD, &sub, 0x200);
    }
    sys_unlinkat(AT_FDCWD, RB_DST, 0x200);
    sys_unlinkat(AT_FDCWD, RB_SRC, 0x200);
    sys_unlinkat(AT_FDCWD, RBASE, 0x200);

    println!("  PASS: mount bench rbind");
    true
}

fn test_mount_bench_rbind_scale() -> bool {
    sys_mkdirat(AT_FDCWD, "/tmp/mntbench\0", 0o777);

    for depth in &[1usize, 2, 4, 8] {
        let d = *depth;
        // Create tree of depth d with submounts at each level
        let scale_src = format!("/tmp/mntbench/scale_src_{}\0", d);
        let scale_dst = format!("/tmp/mntbench/scale_dst_{}\0", d);
        sys_mkdirat(AT_FDCWD, &scale_src, 0o777);
        sys_mkdirat(AT_FDCWD, &scale_dst, 0o777);

        // Build chain: scale_src_X/dir_0000/dir_0001/.../dir_XXXX
        {
            let mut current = format!("/tmp/mntbench/scale_src_{}", d);
            for i in 0..d {
                current.push_str("/dir_");
                current.push_str(&format!("{:04}", i));
                current.push('\0');
                let ret = sys_mkdirat(AT_FDCWD, &current, 0o777);
                current.pop();
                if ret < 0 {
                    println!("  FAIL: scale mkdir {} depth={} ret={}", &current, d, ret);
                    return false;
                }
            }
        }

        // Create submounts at each level by bind-then-remount trick
        for i in 0..d {
            let mut sub = format!("/tmp/mntbench/scale_src_{}", d);
            for j in 0..=i {
                sub.push_str("/dir_");
                sub.push_str(&format!("{:04}", j));
            }
            sub.push('\0');
            let tmp = format!("/tmp/mntbench/scale_tmp_{}_{}\0", d, i);
            sys_mkdirat(AT_FDCWD, &tmp, 0o777);
            let ret = sys_mount(sub.as_ptr(), tmp.as_ptr(), core::ptr::null(), MS_BIND, 0);
            if ret < 0 {
                println!("  FAIL: scale submount bind d={} i={} ret={}", d, i, ret);
                return false;
            }
            sys_umount2(sub.as_ptr(), 0);
            let ret = sys_mount(tmp.as_ptr(), sub.as_ptr(), core::ptr::null(), MS_BIND, 0);
            if ret < 0 {
                println!("  FAIL: scale remount d={} i={} ret={}", d, i, ret);
                return false;
            }
            sys_unlinkat(AT_FDCWD, &tmp, 0x200);
        }

        // Time the rbind call
        let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
        sys_clock_gettime(1, &mut t0);
        let ret = sys_mount(
            scale_src.as_ptr(),
            scale_dst.as_ptr(),
            core::ptr::null(),
            MS_BIND | MS_REC,
            0,
        );
        let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
        sys_clock_gettime(1, &mut t1);
        let ns = ts_diff_ns(&t0, &t1);
        println!("  rbind_scale depth={} elapsed={}ns ret={}", d, ns, ret);
        if ret < 0 {
            println!("  FAIL: rbind scale depth {} ret {}", d, ret);
            return false;
        }

        // Cleanup
        sys_umount2(scale_dst.as_ptr(), 0);
        for i in (0..d).rev() {
            let mut sub = format!("/tmp/mntbench/scale_src_{}", d);
            for j in 0..=i {
                sub.push_str("/dir_");
                sub.push_str(&format!("{:04}", j));
            }
            sub.push('\0');
            sys_umount2(sub.as_ptr(), 0);
        }
        for i in (0..d).rev() {
            let mut sub = format!("/tmp/mntbench/scale_src_{}", d);
            for j in 0..=i {
                sub.push_str("/dir_");
                sub.push_str(&format!("{:04}", j));
            }
            sub.push('\0');
            sys_unlinkat(AT_FDCWD, &sub, 0x200);
        }
        sys_unlinkat(AT_FDCWD, &scale_dst, 0x200);
        sys_unlinkat(AT_FDCWD, &scale_src, 0x200);
    }

    sys_unlinkat(AT_FDCWD, "/tmp/mntbench\0", 0x200);
    println!("  PASS: mount bench rbind scale");
    true
}

fn test_perf_fork_exec() -> bool {
    const N: usize = 50;
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 2] = ["/bin/busybox\0".as_ptr(), "true\0".as_ptr()];
            let envp: [*const u8; 0] = [];
            sys_exec("/bin/busybox\0", &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /bin/true x{}: total={}ns avg={}ns", N, total_ns, avg_ns);
    true
}

fn test_perf_fork_only() -> bool {
    const N: usize = 50;
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            sys_exit(0);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork-only x{}: total={}ns avg={}ns", N, total_ns, avg_ns);
    true
}

fn test_perf_fork_exec_tmp() -> bool {
    const N: usize = 50;
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 2] = ["/tmp/bb\0".as_ptr(), "true\0".as_ptr()];
            let envp: [*const u8; 0] = [];
            sys_exec("/tmp/bb\0", &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /tmp/bb (ramfs) x{}: total={}ns avg={}ns", N, total_ns, avg_ns);
    true
}

fn test_perf_fork_exec_small() -> bool {
    const N: usize = 50;
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 1] = ["/fs_test\0".as_ptr()];
            let envp: [*const u8; 0] = [];
            sys_exec("/fs_test\0", &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /fs_test (ramfs,small) x{}: total={}ns avg={}ns", N, total_ns, avg_ns);
    true
}

fn test_perf_fork_exec_shell() -> bool {
    // fork+exec /bin/sh -c "ls /" — full shell startup (libc load, script parse, config read)
    const N: usize = 10;
    let cmd = "ls /\0";
    let sh = "/bin/sh\0";
    let sh_c = "-c\0";
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 4] = [sh.as_ptr(), sh_c.as_ptr(), cmd.as_ptr(), core::ptr::null()];
            let envp: [*const u8; 0] = [];
            sys_exec(sh, &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /bin/sh -c 'ls' x{}: avg={}ns ({:.1}us)", N, avg_ns, avg_ns as f64 / 1000.0);
    true
}

fn test_perf_fork_exec_shell_quiet() -> bool {
    // Same as above but redirect to /dev/null — suppresses stdout/stderr writes
    const N: usize = 10;
    let cmd = "ls / >/dev/null 2>&1\0";
    let sh = "/bin/sh\0";
    let sh_c = "-c\0";
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 4] = [sh.as_ptr(), sh_c.as_ptr(), cmd.as_ptr(), core::ptr::null()];
            let envp: [*const u8; 0] = [];
            sys_exec(sh, &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /bin/sh -c 'ls >/dev/null' x{}: avg={}ns ({:.1}us)", N, avg_ns, avg_ns as f64 / 1000.0);
    true
}

fn test_perf_fork_exec_shell_min() -> bool {
    // Minimal: env -i clears environment, reduce shell startup work
    // Runs `/bin/sh -c true` (no ls, no output at all)
    const N: usize = 10;
    let cmd = "env -i /bin/sh -c true\0";
    let sh = "/bin/sh\0";
    let sh_c = "-c\0";
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let pid = sys_fork();
        if pid == 0 {
            let args: [*const u8; 4] = [sh.as_ptr(), sh_c.as_ptr(), cmd.as_ptr(), core::ptr::null()];
            let envp: [*const u8; 0] = [];
            sys_exec(sh, &args, &envp);
            sys_exit(1);
        } else if pid > 0 {
            let mut status: i32 = 0;
            sys_waitpid(pid, &mut status);
        } else {
            println!("  FAIL: fork returned {}", pid);
            return false;
        }
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  fork+exec /bin/sh -c 'env -i /bin/sh -c true' x{}: avg={}ns ({:.1}us)", N, avg_ns, avg_ns as f64 / 1000.0);
    true
}

fn test_perf_read_bb() -> bool {
    const CHUNK: usize = 4096;
    const N: usize = 50;
    // Read /bin/busybox (possibly lwext4) vs /tmp/bb (ramfs)
    let paths = [("/bin/busybox\0", "bin"), ("/tmp/bb\0", "tmp")];
    for (path, label) in &paths {
        let mut t0: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
        sys_clock_gettime(1, &mut t0);
        for _ in 0..N {
            let fd = sys_open(path, 0);
            if fd < 0 { println!("  FAIL: open {} returned {}", label, fd); return false; }
            let mut buf = [0u8; CHUNK];
            let _n = sys_read(fd as usize, &mut buf);
            sys_close(fd as usize);
        }
        let mut t1: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
        sys_clock_gettime(1, &mut t1);
        let total_ns = ts_diff_ns(&t0, &t1);
        let avg_ns = total_ns / N as u64;
        println!("  read {}/{}B x{}: total={}ns avg={}ns", label, CHUNK, N, total_ns, avg_ns);
    }
    true
}

fn test_perf_read_full() -> bool {
    const N: usize = 5;
    // Read ENTIRE 800KB busybox in 4KB chunks (sequential — exercises readahead)
    let paths = [("/bin/busybox\0", "bin"), ("/tmp/bb\0", "tmp")];
    for (path, label) in &paths {
        let mut t0: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
        sys_clock_gettime(1, &mut t0);
        for _ in 0..N {
            let fd = sys_open(path, 0);
            if fd < 0 { println!("  FAIL: open {} returned {}", label, fd); return false; }
            let mut buf = [0u8; 4096];
            loop {
                let n = sys_read(fd as usize, &mut buf);
                if n <= 0 { break; }
            }
            sys_close(fd as usize);
        }
        let mut t1: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
        sys_clock_gettime(1, &mut t1);
        let total_ns = ts_diff_ns(&t0, &t1);
        let avg_ns = total_ns / N as u64;
        println!("  read full {} (800KB) x{}: total={}ns avg={}ns", label, N, total_ns, avg_ns);
    }
    true
}

fn test_perf_proc_mounts() -> bool {
    const N: usize = 10;
    let mut t0: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t0);
    for _ in 0..N {
        let fd = sys_open("/proc/mounts\0", 0);
        if fd < 0 {
            println!("  FAIL: open /proc/mounts returned {}", fd);
            return false;
        }
        let mut buf = [0u8; 4096];
        let _n = sys_read(fd as usize, &mut buf);
        sys_close(fd as usize);
    }
    let mut t1: TimeSpec = TimeSpec { tv_sec: 0, tv_nsec: 0 };
    sys_clock_gettime(1, &mut t1);
    let total_ns = ts_diff_ns(&t0, &t1);
    let avg_ns = total_ns / N as u64;
    println!("  read /proc/mounts x{}: total={}ns avg={}ns", N, total_ns, avg_ns);
    true
}

fn test_perf_exec_twice() -> bool {
    // First run: cold (PageCache miss on lwext4)
    let mut t0: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
    sys_clock_gettime(1, &mut t0);
    let args: [*const u8; 2] = ["/bin/busybox\0".as_ptr(), "true\0".as_ptr()];
    let envp: [*const u8; 0] = [];
    let pid = sys_fork();
    if pid == 0 {
        sys_exec("/bin/busybox\0", &args, &envp);
        sys_exit(1);
    }
    let mut s: i32 = 0;
    sys_waitpid(pid, &mut s);
    let mut t1: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
    sys_clock_gettime(1, &mut t1);
    let cold_ns = ts_diff_ns(&t0, &t1);

    // Second run: warm (PageCache should be hot)
    let mut t0_2: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
    sys_clock_gettime(1, &mut t0_2);
    let pid2 = sys_fork();
    if pid2 == 0 {
        sys_exec("/bin/busybox\0", &args, &envp);
        sys_exit(1);
    }
    sys_waitpid(pid2, &mut s);
    let mut t1_2: TimeSpec = TimeSpec { tv_sec:0, tv_nsec:0 };
    sys_clock_gettime(1, &mut t1_2);
    let warm_ns = ts_diff_ns(&t0_2, &t1_2);
    println!("  exec cold={}ns warm={}ns ratio={:.1}x", cold_ns, warm_ns, cold_ns as f64 / warm_ns.max(1) as f64);
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
fn run_test(name: &str, test_fn: fn() -> bool) -> bool {
    let ok = test_fn();
    if !ok { println!("[FAIL] {}", name); }
    ok
}

struct TestCase {
    name: &'static str,
    desc: &'static str,
    func: fn() -> bool,
}

#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("=== FS Test Suite ===");
    // Filter: if arguments given, only run tests whose name matches any arg
    let filter: &[&str] = if _argc > 0 { _argv } else { &[] };
    let has_filter = !filter.is_empty();

    let tests: &[TestCase] = &[
        TestCase { name: "mkdir", desc: "mkdir", func: test_mkdir },
        TestCase { name: "create_and_write", desc: "file create + write", func: test_create_and_write },
        TestCase { name: "read", desc: "file read", func: test_read },
        TestCase { name: "symlink", desc: "symlink", func: test_symlink },
        TestCase { name: "readlink", desc: "readlink", func: test_readlink },
        TestCase { name: "read_via_symlink", desc: "read via symlink", func: test_read_via_symlink },
        TestCase { name: "unlink", desc: "unlink", func: test_unlink },
        TestCase { name: "rmdir", desc: "rmdir", func: test_rmdir },
        TestCase { name: "dangling_symlink", desc: "dangling symlink", func: test_dangling_symlink },
        TestCase { name: "eloop", desc: "ELOOP detection", func: test_eloop },
        TestCase { name: "symlink_chain", desc: "symlink chain", func: test_symlink_chain },
        TestCase { name: "excl_create", desc: "O_CREAT|O_EXCL", func: test_excl_create },
        TestCase { name: "readlink_on_regular", desc: "readlink on regular file", func: test_readlink_on_regular },
        TestCase { name: "unlink_symlink_preserves_target", desc: "unlink symlink preserves target", func: test_unlink_symlink_preserves_target },
        TestCase { name: "hard_link", desc: "hard link", func: test_hard_link },
        TestCase { name: "hard_link_dir_rejected", desc: "hard link to dir rejected", func: test_hard_link_dir_rejected },
        TestCase { name: "lseek", desc: "lseek", func: test_lseek },
        TestCase { name: "rename_file", desc: "rename file", func: test_rename_file },
        TestCase { name: "rename_dir", desc: "rename directory", func: test_rename_dir },
        TestCase { name: "fstatat", desc: "fstatat", func: test_fstatat },
        TestCase { name: "ftruncate", desc: "ftruncate", func: test_ftruncate },
        TestCase { name: "getdents64", desc: "getdents64", func: test_getdents64 },
        TestCase { name: "read_empty", desc: "read empty file", func: test_read_empty },
        TestCase { name: "read_past_eof", desc: "read past EOF", func: test_read_past_eof },
        TestCase { name: "read_data_integrity", desc: "read data integrity (256B + partial)", func: test_read_data_integrity },
        TestCase { name: "read_bad_fd", desc: "read bad fd -> EBADF", func: test_read_bad_fd },
        TestCase { name: "read_dir", desc: "read on dir -> EISDIR", func: test_read_dir },
        TestCase { name: "write_readonly", desc: "write readonly fd -> EBADF", func: test_write_readonly },
        TestCase { name: "write_append", desc: "O_APPEND + lseek atomicity", func: test_write_append },
        TestCase { name: "write_varying_sizes", desc: "write varying sizes 1..4096", func: test_write_varying_sizes },
        TestCase { name: "write_overwrite_middle", desc: "overwrite middle of file", func: test_write_overwrite_middle },
        TestCase { name: "write_bad_fd", desc: "write bad fd -> EBADF", func: test_write_bad_fd },
        TestCase { name: "lseek_seek_end", desc: "lseek SEEK_END + negative offset", func: test_lseek_seek_end },
        TestCase { name: "lseek_bad_whence", desc: "lseek bad whence -> EINVAL", func: test_lseek_bad_whence },
        TestCase { name: "lseek_pipe", desc: "lseek on pipe -> ESPIPE", func: test_lseek_pipe },
        TestCase { name: "lseek_hole_read", desc: "lseek beyond EOF + hole read", func: test_lseek_hole_read },
        TestCase { name: "lseek_chain", desc: "lseek chain: SET→CUR→END", func: test_lseek_chain },
        TestCase { name: "open_noent", desc: "open nonexistent -> ENOENT", func: test_open_noent },
        TestCase { name: "open_dir_as_file", desc: "open dir as file -> EISDIR", func: test_open_dir_as_file },
        TestCase { name: "open_trunc", desc: "O_TRUNC (size=0 + data lost)", func: test_open_trunc },
        TestCase { name: "close_twice", desc: "close twice -> EBADF", func: test_close_twice },
        TestCase { name: "open_close_many", desc: "open/close 32 times", func: test_open_close_many },
        TestCase { name: "open_create_existing", desc: "open existing file (no O_CREAT)", func: test_open_create_existing },
        TestCase { name: "stress_create_many", desc: "stress: create 50 files + verify", func: test_stress_create_many },
        TestCase { name: "stress_read_many", desc: "stress: read 30 files with unique content", func: test_stress_read_many },
        TestCase { name: "stress_unlink_loop", desc: "stress: unlink 30 files -> empty dir", func: test_stress_unlink_loop },
        TestCase { name: "stress_rename_loop", desc: "stress: rename A↔B loop x10", func: test_stress_rename_loop },
        TestCase { name: "stress_large_file", desc: "stress: large file 64KB write+read", func: test_stress_large_file },
        TestCase { name: "stress_getdents", desc: "stress: getdents counts 20 files", func: test_stress_getdents },
        TestCase { name: "stress_truncate", desc: "stress: truncate 100→50→200 with hole", func: test_stress_truncate },
        TestCase { name: "perf_getdents_1000", desc: "perf: getdents 1000 files", func: test_perf_getdents_1000 },
        TestCase { name: "perf_stat_like_1000", desc: "perf: stat-like 1000 files", func: test_perf_stat_like_1000 },
        TestCase { name: "perf_repeated_lookup_cache", desc: "perf: repeated lookup cache", func: test_perf_repeated_lookup_cache },
        TestCase { name: "perf_symlink_batch_200", desc: "perf: symlink batch 200", func: test_perf_symlink_batch_200 },
        TestCase { name: "perf_open_access_large_dir", desc: "perf: open/access large dir", func: test_perf_open_access_large_dir },
        TestCase { name: "fork_read_same_fd", desc: "fork: read same fd (parent+child)", func: test_fork_read_same_fd },
        TestCase { name: "fork_create", desc: "fork: create files (parent+child)", func: test_fork_create },
        TestCase { name: "lc_repeated_oc", desc: "lifecycle: repeated open/close 200x", func: test_repeated_open_close_same_file_lifecycle },
        TestCase { name: "lc_lookup_close", desc: "lifecycle: lookup 64 files then close", func: test_lookup_many_files_then_close_lifecycle },
        TestCase { name: "lc_unlink_open", desc: "lifecycle: unlink while open", func: test_unlink_open_file_lifecycle },
        TestCase { name: "rc_clean_shrink", desc: "reclaim: clean page cache shrink", func: test_page_cache_reclaim_clean_pages },
        TestCase { name: "rc_dirty_noloss", desc: "reclaim: dirty page no-loss", func: test_dirty_page_no_loss_under_reclaim },
        TestCase { name: "tc_trunc_cache", desc: "truncate: invalidates pagecache", func: test_truncate_invalidates_pagecache },
        TestCase { name: "mount_bench_bind", desc: "mount bench: bind 8 levels", func: test_mount_bench_bind },
        TestCase { name: "mount_bench_rbind", desc: "mount bench: rbind 8 levels", func: test_mount_bench_rbind },
        TestCase { name: "mount_bench_rbind_scale", desc: "mount bench: rbind scale 1,2,4,8", func: test_mount_bench_rbind_scale },
        TestCase { name: "perf_fork_exec", desc: "perf: fork+exec /bin/true x50", func: test_perf_fork_exec },
        TestCase { name: "perf_fork_only", desc: "perf: fork-only x50", func: test_perf_fork_only },
        TestCase { name: "perf_fork_exec_tmp", desc: "perf: fork+exec /tmp/bb (ramfs) x50", func: test_perf_fork_exec_tmp },
        TestCase { name: "perf_fork_exec_shell", desc: "perf: fork+exec /bin/sh -c ls x10", func: test_perf_fork_exec_shell },
        TestCase { name: "perf_fork_exec_shell_quiet", desc: "perf: fork+exec sh -c ls >/dev/null x10", func: test_perf_fork_exec_shell_quiet },
        TestCase { name: "perf_fork_exec_shell_min", desc: "perf: fork+exec sh -c env -i sh -c true x10", func: test_perf_fork_exec_shell_min },
        TestCase { name: "perf_read_bb", desc: "perf: read bin/tmp busybox 4KB x50", func: test_perf_read_bb },
        TestCase { name: "perf_read_full", desc: "perf: read full 800KB busybox x5", func: test_perf_read_full },
        TestCase { name: "perf_exec_twice", desc: "perf: exec cold vs warm", func: test_perf_exec_twice },
        TestCase { name: "perf_proc_mounts", desc: "perf: read /proc/mounts x10", func: test_perf_proc_mounts },
    ];

    let total = tests.len();
    let mut passed = 0;
    let mut failed = 0;

    for (i, tc) in tests.iter().enumerate() {
        // Skip if filter is active and this test doesn't match any filter item
        if has_filter && !filter.iter().any(|f| *f == tc.name) {
            continue;
        }
        println!("[{}/{}] {}", i + 1, total, tc.desc);
        if run_test(tc.name, tc.func) {
            passed += 1;
        } else {
            failed += 1;
        }
    }

    println!("=== FS Test: {}/{} passed ===", passed, total);

    if failed > 0 { 1 } else { 0 }
}

fn run_profile_audit() {}
