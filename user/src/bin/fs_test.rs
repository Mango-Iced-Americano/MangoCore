#![no_std]
#![no_main]

extern crate alloc;
use user_lib::syscall::*;
use user_lib::{println};

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

    let bytes = &buf[..n as usize];
    let mut found_dot = false;
    let mut found_dotdot = false;
    let mut found_x = false;

    let mut pos = 0;
    while pos + 19 <= bytes.len() {
        let d_reclen = u16::from_le_bytes([bytes[pos + 16], bytes[pos + 17]]) as usize;
        let d_type = bytes[pos + 18];
        let name_start = pos + 19;
        let name_end = bytes[name_start..].iter().position(|&b| b == 0).map(|i| name_start + i).unwrap_or(bytes.len());
        let name = core::str::from_utf8(&bytes[name_start..name_end]).unwrap_or("???");
        match name {
            "." => found_dot = true,
            ".." => found_dotdot = true,
            "x" => { found_x = true; if d_type != 8 { println!("  FAIL: 'x' d_type={} (expected DT_REG=8)", d_type); return false; } }
            _ => {}
        }
        if d_reclen == 0 { break; }
        pos += d_reclen;
    }

    if !found_dot || !found_dotdot || !found_x {
        println!("  FAIL: missing entries: .={} ..={} x={}", found_dot, found_dotdot, found_x);
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
    const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp16/empty\0", O_CREAT);
    sys_close(fd as usize);
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
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp17/short\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"hello");
    sys_close(fd as usize);
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
    const O_CREAT: u32 = 0o100; const O_WRONLY: u32 = 0o1; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp20/ro\0", O_CREAT | O_WRONLY);
    sys_write(fd as usize, b"data");
    sys_close(fd as usize);
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
    sys_write(fd as usize, b"DATA_AT_50");
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
    const O_CREAT: u32 = 0o100; const O_RDWR: u32 = 0o2; const O_RDONLY: u32 = 0;
    let fd = sys_open("/tmp32/exist\0", O_CREAT | O_RDWR);
    sys_write(fd as usize, b"original content");
    sys_close(fd as usize);
    let fd = sys_open("/tmp32/exist\0", O_RDONLY); // no O_CREAT, no O_TRUNC
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
    let fd = sys_open("/tmp35\0", 0x200000 | 0);
    let mut buf = [0u8; 512];
    let n = sys_getdents64(fd as usize, &mut buf);
    sys_close(fd as usize);
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
    let fd = sys_open("/tmp38\0", 0x200000 | 0);
    let mut buf = [0u8; 512]; // small buffer — forces multiple getdents64 calls
    let mut entries = 0usize;
    loop {
        let n = sys_getdents64(fd as usize, &mut buf);
        if n <= 0 { break; }
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

    println!("[1/51] mkdir");
    if test_mkdir() { passed += 1; } else { failed += 1; }

    println!("[2/51] file create + write");
    if test_create_and_write() { passed += 1; } else { failed += 1; }

    println!("[3/51] file read");
    if test_read() { passed += 1; } else { failed += 1; }

    println!("[4/51] symlink");
    if test_symlink() { passed += 1; } else { failed += 1; }

    println!("[5/51] readlink");
    if test_readlink() { passed += 1; } else { failed += 1; }

    println!("[6/51] read via symlink");
    if test_read_via_symlink() { passed += 1; } else { failed += 1; }

    println!("[7/51] unlink + rmdir");
    if test_unlink() && test_rmdir() { passed += 1; } else { failed += 1; }

    println!("[8/51] dangling symlink");
    if test_dangling_symlink() { passed += 1; } else { failed += 1; }

    println!("[9/51] ELOOP detection");
    if test_eloop() { passed += 1; } else { failed += 1; }

    println!("[10/51] symlink chain");
    if test_symlink_chain() { passed += 1; } else { failed += 1; }

    println!("[11/51] O_CREAT|O_EXCL");
    if test_excl_create() { passed += 1; } else { failed += 1; }

    println!("[12/51] readlink on regular file");
    if test_readlink_on_regular() { passed += 1; } else { failed += 1; }

    println!("[13/51] unlink symlink preserves target");
    if test_unlink_symlink_preserves_target() { passed += 1; } else { failed += 1; }

    println!("[14/51] hard link");
    if test_hard_link() { passed += 1; } else { failed += 1; }

    println!("[15/51] hard link to dir rejected");
    if test_hard_link_dir_rejected() { passed += 1; } else { failed += 1; }

    println!("[16/51] lseek");
    if test_lseek() { passed += 1; } else { failed += 1; }

    println!("[17/51] rename file");
    if test_rename_file() { passed += 1; } else { failed += 1; }

    println!("[18/51] rename directory");
    if test_rename_dir() { passed += 1; } else { failed += 1; }

    println!("[19/51] fstatat");
    if test_fstatat() { passed += 1; } else { failed += 1; }

    println!("[20/51] ftruncate");
    if test_ftruncate() { passed += 1; } else { failed += 1; }

    println!("[21/51] getdents64");
    if test_getdents64() { passed += 1; } else { failed += 1; }

    // ── A组: 高级 read/write 测试 ──────────────────────────

    println!("[22/51] read empty file");
    if test_read_empty() { passed += 1; } else { failed += 1; }

    println!("[23/51] read past EOF");
    if test_read_past_eof() { passed += 1; } else { failed += 1; }

    println!("[24/51] read data integrity (256B + partial)");
    if test_read_data_integrity() { passed += 1; } else { failed += 1; }

    println!("[25/51] read bad fd -> EBADF");
    if test_read_bad_fd() { passed += 1; } else { failed += 1; }

    println!("[26/51] read on dir -> EISDIR");
    if test_read_dir() { passed += 1; } else { failed += 1; }

    println!("[27/51] write readonly fd -> EBADF");
    if test_write_readonly() { passed += 1; } else { failed += 1; }

    println!("[28/51] O_APPEND + lseek atomicity");
    if test_write_append() { passed += 1; } else { failed += 1; }

    println!("[29/51] write varying sizes 1..4096");
    if test_write_varying_sizes() { passed += 1; } else { failed += 1; }

    println!("[30/51] overwrite middle of file");
    if test_write_overwrite_middle() { passed += 1; } else { failed += 1; }

    println!("[31/51] write bad fd -> EBADF");
    if test_write_bad_fd() { passed += 1; } else { failed += 1; }

    // ── B组: 高级 lseek 测试 ──────────────────────────────

    println!("[32/51] lseek SEEK_END + negative offset");
    if test_lseek_seek_end() { passed += 1; } else { failed += 1; }

    println!("[33/51] lseek bad whence -> EINVAL");
    if test_lseek_bad_whence() { passed += 1; } else { failed += 1; }

    println!("[34/51] lseek on pipe -> ESPIPE");
    if test_lseek_pipe() { passed += 1; } else { failed += 1; }

    println!("[35/51] lseek beyond EOF + hole read");
    if test_lseek_hole_read() { passed += 1; } else { failed += 1; }

    println!("[36/51] lseek chain: SET→CUR→END");
    if test_lseek_chain() { passed += 1; } else { failed += 1; }

    // ── C组: open/close 错误路径 ───────────────────────────

    println!("[37/51] open nonexistent -> ENOENT");
    if test_open_noent() { passed += 1; } else { failed += 1; }

    println!("[38/51] open dir as file -> EISDIR");
    if test_open_dir_as_file() { passed += 1; } else { failed += 1; }

    println!("[39/51] O_TRUNC (size=0 + data lost)");
    if test_open_trunc() { passed += 1; } else { failed += 1; }

    println!("[40/51] close twice -> EBADF");
    if test_close_twice() { passed += 1; } else { failed += 1; }

    println!("[41/51] open/close 32 times");
    if test_open_close_many() { passed += 1; } else { failed += 1; }

    println!("[42/51] open existing file (no O_CREAT)");
    if test_open_create_existing() { passed += 1; } else { failed += 1; }

    // ── D组: 压力/边界测试 ─────────────────────────────────

    println!("[43/51] stress: create 50 files + verify");
    if test_stress_create_many() { passed += 1; } else { failed += 1; }

    println!("[44/51] stress: read 30 files with unique content");
    if test_stress_read_many() { passed += 1; } else { failed += 1; }

    println!("[45/51] stress: unlink 30 files -> empty dir");
    if test_stress_unlink_loop() { passed += 1; } else { failed += 1; }

    println!("[46/51] stress: rename A↔B loop x10");
    if test_stress_rename_loop() { passed += 1; } else { failed += 1; }

    println!("[47/51] stress: large file 64KB write+read");
    if test_stress_large_file() { passed += 1; } else { failed += 1; }

    println!("[48/51] stress: getdents counts 20 files");
    if test_stress_getdents() { passed += 1; } else { failed += 1; }

    println!("[49/51] stress: truncate 100→50→200 with hole");
    if test_stress_truncate() { passed += 1; } else { failed += 1; }

    // ── E组: 并发测试 (fork) ──────────────────────────────

    println!("[50/51] fork: read same fd (parent+child)");
    if test_fork_read_same_fd() { passed += 1; } else { failed += 1; }

    println!("[51/51] fork: create files (parent+child)");
    if test_fork_create() { passed += 1; } else { failed += 1; }

    println!("=== FS Test: {}/{} passed ===", passed, passed + failed);

    /*
    // ── TTY diagnostics (comment in to debug echo) ─────────────────
    println!("=== TTY Diagnostics ===");
    tty_diag();
    */

    0
}

/*
fn tty_diag() {
    use user_lib::syscall::{sys_ioctl, sys_open, sys_close, sys_write, TCGETS, Termios};
    let mut t = Termios { iflag: 0, oflag: 0, cflag: 0, lflag: 0, line: 0, cc: [0; 19] };
    let fd = sys_open("/dev/tty\0", 0);
    if fd < 0 {
        println!("  TTY open failed: {}", fd);
        return;
    }
    let ret = sys_ioctl(fd as usize, TCGETS, &mut t as *mut Termios as usize);
    if ret < 0 {
        println!("  TCGETS failed: {}", ret);
    } else {
        println!("  termios lflag=0o{:o} ECHO={} ICANON={} ISIG={} cc[VEOF]={} cc[VEOL]={}",
            t.lflag, t.has_echo(), t.has_icanon(), t.has_isig(), t.cc[4], t.cc[11]);
    }
    sys_close(fd as usize);

    let mut t2 = Termios { iflag: 0, oflag: 0, cflag: 0, lflag: 0, line: 0, cc: [0; 19] };
    let ret = sys_ioctl(0, TCGETS, &mut t2 as *mut Termios as usize);
    if ret < 0 {
        println!("  TCGETS on fd 0 failed: {}", ret);
    } else {
        println!("  fd0 termios lflag=0o{:o} ECHO={} ICANON={} ISIG={}",
            t2.lflag, t2.has_echo(), t2.has_icanon(), t2.has_isig());
    }

    println!("=== TTY Diagnostics Done ===");
}
*/
