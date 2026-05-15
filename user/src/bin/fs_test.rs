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
    let fd = sys_open("/tmp15/a\0", O_CREAT | O_WRONLY);
    if fd >= 0 { sys_close(fd as usize); }
    let fd = sys_open("/tmp15/b\0", O_CREAT | O_WRONLY);
    if fd >= 0 { sys_close(fd as usize); }
    sys_mkdirat(AT_FDCWD, "/tmp15/subdir\0", 0o777);

    const O_RDONLY: u32 = 0;
    // Open directory with O_RDONLY|O_DIRECTORY
    let fd = sys_open("/tmp15\0", O_RDONLY | 0o200000);
    if fd < 0 {
        println!("  FAIL: open dir for getdents returned {}", fd);
        return false;
    }

    let mut buf = [0u8; 512];
    let n = sys_getdents64(fd as usize, &mut buf);
    sys_close(fd as usize);

    if n <= 0 {
        println!("  FAIL: getdents64 returned {}", n);
        return false;
    }

    // Parse dirent64 entries to find expected names
    let bytes = &buf[..n as usize];
    let mut found_dot = false;
    let mut found_dotdot = false;
    let mut found_a = false;
    let mut found_b = false;
    let mut found_subdir = false;

    let mut pos = 0;
    while pos < bytes.len() {
        // dirent64: d_ino(u64), d_off(i64), d_reclen(u16), d_type(u8), d_name(...)
        if pos + 19 > bytes.len() { break; }
        let d_reclen = u16::from_le_bytes([bytes[pos + 16], bytes[pos + 17]]) as usize;
        let d_type = bytes[pos + 18];
        let name_start = pos + 19;
        let name_end = bytes[name_start..].iter().position(|&b| b == 0).map(|i| name_start + i).unwrap_or(bytes.len());
        let name = core::str::from_utf8(&bytes[name_start..name_end]).unwrap_or("???");

        match name {
            "." => found_dot = true,
            ".." => found_dotdot = true,
            "a" => { found_a = true; if d_type != 8 { println!("  FAIL: 'a' type={} (expected DT_REG=8)", d_type); return false; } }
            "b" => { found_b = true; if d_type != 8 { println!("  FAIL: 'b' type={} (expected DT_REG=8)", d_type); return false; } }
            "subdir" => { found_subdir = true; if d_type != 4 { println!("  FAIL: 'subdir' type={} (expected DT_DIR=4)", d_type); return false; } }
            _ => {}
        }
        pos += d_reclen;
        if d_reclen == 0 { break; }
    }

    if !found_dot || !found_dotdot || !found_a || !found_b || !found_subdir {
        println!("  FAIL: getdents missing entries: .={} ..={} a={} b={} subdir={}",
            found_dot, found_dotdot, found_a, found_b, found_subdir);
        return false;
    }
    println!("  PASS: getdents64 OK (found . .. a b subdir)");

    sys_unlinkat(AT_FDCWD, "/tmp15/a\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp15/b\0", 0);
    sys_unlinkat(AT_FDCWD, "/tmp15/subdir\0", 0x200);
    sys_unlinkat(AT_FDCWD, "/tmp15\0", 0x200);
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

    println!("[1/21] mkdir");
    if test_mkdir() { passed += 1; } else { failed += 1; }

    println!("[2/21] file create + write");
    if test_create_and_write() { passed += 1; } else { failed += 1; }

    println!("[3/21] file read");
    if test_read() { passed += 1; } else { failed += 1; }

    println!("[4/21] symlink");
    if test_symlink() { passed += 1; } else { failed += 1; }

    println!("[5/21] readlink");
    if test_readlink() { passed += 1; } else { failed += 1; }

    println!("[6/21] read via symlink");
    if test_read_via_symlink() { passed += 1; } else { failed += 1; }

    println!("[7/21] unlink + rmdir");
    if test_unlink() && test_rmdir() { passed += 1; } else { failed += 1; }

    println!("[8/21] dangling symlink");
    if test_dangling_symlink() { passed += 1; } else { failed += 1; }

    println!("[9/21] ELOOP detection");
    if test_eloop() { passed += 1; } else { failed += 1; }

    println!("[10/21] symlink chain");
    if test_symlink_chain() { passed += 1; } else { failed += 1; }

    println!("[11/21] O_CREAT|O_EXCL");
    if test_excl_create() { passed += 1; } else { failed += 1; }

    println!("[12/21] readlink on regular file");
    if test_readlink_on_regular() { passed += 1; } else { failed += 1; }

    println!("[13/21] unlink symlink preserves target");
    if test_unlink_symlink_preserves_target() { passed += 1; } else { failed += 1; }

    println!("[14/21] hard link");
    if test_hard_link() { passed += 1; } else { failed += 1; }

    println!("[15/21] hard link to dir rejected");
    if test_hard_link_dir_rejected() { passed += 1; } else { failed += 1; }

    println!("[16/21] lseek");
    if test_lseek() { passed += 1; } else { failed += 1; }

    println!("[17/21] rename file");
    if test_rename_file() { passed += 1; } else { failed += 1; }

    println!("[18/21] rename directory");
    if test_rename_dir() { passed += 1; } else { failed += 1; }

    println!("[19/21] fstatat");
    if test_fstatat() { passed += 1; } else { failed += 1; }

    println!("[20/21] ftruncate");
    if test_ftruncate() { passed += 1; } else { failed += 1; }

    println!("[21/21] getdents64");
    if test_getdents64() { passed += 1; } else { failed += 1; }

    println!("=== FS Test: {}/{} passed ===", passed, passed + failed);
    0
}
