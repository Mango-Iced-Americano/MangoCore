//! Regression: lwext4 namespace mutations must preserve POSIX inode lifetime.
//!
//! This test intentionally targets `/sdcard`, which is the formatted ext4
//! fixture used by the QEMU L4 regression gate.  Do not run the regression
//! image on a board whose persistent SSD has not been backed up.
//!
//! Covered invariants:
//! - `rmdir()` on a non-empty directory returns `ENOTEMPTY` without deleting it;
//! - renaming two hard links to the same inode is a no-op;
//! - chmod through one hard-link alias is immediately visible through another;
//! - failed `RENAME_NOREPLACE` and non-empty-directory replacement preserve
//!   both namespaces;
//! - overwrite-rename keeps an already-open target inode alive and isolated;
//! - an fd opened before rename follows the inode, not its former path;
//! - unlink followed by same-name creation does not alias the still-open inode.

use user_lib::println;
use user_lib::syscall::*;

const BASE: &str = "/sdcard/reg_lwext4_namespace\0";

const RMDIR_DIR: &str = "/sdcard/reg_lwext4_namespace/nonempty\0";
const RMDIR_CHILD: &str = "/sdcard/reg_lwext4_namespace/nonempty/child\0";

const SAME_SRC: &str = "/sdcard/reg_lwext4_namespace/same_src\0";
const SAME_DST: &str = "/sdcard/reg_lwext4_namespace/same_dst\0";

const META_SRC: &str = "/sdcard/reg_lwext4_namespace/meta_src\0";
const META_DST: &str = "/sdcard/reg_lwext4_namespace/meta_dst\0";

const NR_SRC: &str = "/sdcard/reg_lwext4_namespace/noreplace_src\0";
const NR_DST: &str = "/sdcard/reg_lwext4_namespace/noreplace_dst\0";

const DIR_SRC: &str = "/sdcard/reg_lwext4_namespace/dir_src\0";
const DIR_DST: &str = "/sdcard/reg_lwext4_namespace/dir_dst\0";
const DIR_DST_CHILD: &str = "/sdcard/reg_lwext4_namespace/dir_dst/child\0";

const EMPTY_SRC: &str = "/sdcard/reg_lwext4_namespace/empty_src\0";
const EMPTY_DST: &str = "/sdcard/reg_lwext4_namespace/empty_dst\0";

const DIRFD_OLD: &str = "/sdcard/reg_lwext4_namespace/dirfd_old\0";
const DIRFD_NEW: &str = "/sdcard/reg_lwext4_namespace/dirfd_new\0";
const DIRFD_CHILD: &str = "/sdcard/reg_lwext4_namespace/dirfd_new/via_fd\0";
const DIRFD_CHILD_NAME: &[u8] = b"via_fd\0";

const OVER_SRC: &str = "/sdcard/reg_lwext4_namespace/overwrite_src\0";
const OVER_DST: &str = "/sdcard/reg_lwext4_namespace/overwrite_dst\0";

const MOVE_SRC: &str = "/sdcard/reg_lwext4_namespace/move_src\0";
const MOVE_DST: &str = "/sdcard/reg_lwext4_namespace/move_dst\0";

const UNLINK_PATH: &str = "/sdcard/reg_lwext4_namespace/unlink_reuse\0";

const AT_FDCWD: isize = -100;
const AT_REMOVEDIR: u32 = 0x200;
const O_RDONLY: u32 = 0;
const O_RDWR: u32 = 0o2;
const O_CREAT: u32 = 0o100;
const O_TRUNC: u32 = 0o1000;
const SYSCALL_FCHMOD: usize = 52;
const RENAME_NOREPLACE: u32 = 1;
const EEXIST: isize = -17;
const ENOENT: isize = -2;
const ENOTEMPTY: isize = -39;
const EOPNOTSUPP: isize = -95;

fn cleanup_fixture() {
    // Files first, then directories in leaf-to-root order.  Every operation
    // is deliberately scoped below BASE; failures are harmless on a fresh
    // QEMU fixture and must never broaden into recursive cleanup.
    for path in [
        RMDIR_CHILD,
        SAME_SRC,
        SAME_DST,
        META_SRC,
        META_DST,
        NR_SRC,
        NR_DST,
        DIR_DST_CHILD,
        DIRFD_CHILD,
        OVER_SRC,
        OVER_DST,
        MOVE_SRC,
        MOVE_DST,
        UNLINK_PATH,
    ] {
        let _ = sys_unlinkat(AT_FDCWD, path, 0);
    }
    for path in [
        RMDIR_DIR,
        DIR_SRC,
        DIR_DST,
        EMPTY_SRC,
        EMPTY_DST,
        DIRFD_OLD,
        DIRFD_NEW,
        BASE,
    ] {
        let _ = sys_unlinkat(AT_FDCWD, path, AT_REMOVEDIR);
    }
}

fn empty_stat() -> Stat {
    Stat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 0,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad: 0,
        st_size: 0,
        st_blksize: 0,
        __pad2: 0,
        st_blocks: 0,
        st_atime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        st_mtime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        st_ctime: TimeSpec { tv_sec: 0, tv_nsec: 0 },
        __unused: 0,
    }
}

fn create_file(path: &str, data: &[u8]) -> Result<usize, isize> {
    let fd = sys_open(path, O_CREAT | O_RDWR | O_TRUNC);
    if fd < 0 {
        return Err(fd);
    }
    let fd = fd as usize;
    let wrote = sys_write(fd, data);
    if wrote != data.len() as isize {
        let _ = sys_close(fd);
        return Err(if wrote < 0 { wrote } else { -5 });
    }
    let synced = sys_fsync(fd);
    if synced < 0 {
        let _ = sys_close(fd);
        return Err(synced);
    }
    Ok(fd)
}

fn create_file_closed(path: &str, data: &[u8]) -> bool {
    match create_file(path, data) {
        Ok(fd) => sys_close(fd) == 0,
        Err(err) => {
            println!("  create {} failed: {}", path, err);
            false
        }
    }
}

fn sys_fchmod_raw(fd: usize, mode: u32) -> isize {
    #[cfg(target_arch = "riscv64")]
    {
        let mut ret: isize;
        unsafe {
            core::arch::asm!(
                "ecall",
                inlateout("x10") fd => ret,
                in("x11") mode as usize,
                in("x12") 0usize,
                in("x17") SYSCALL_FCHMOD,
            );
        }
        ret
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        __syscall(SYSCALL_FCHMOD, fd, mode as usize, 0, 0, 0, 0)
    }
}

fn read_fd_exact(fd: usize, offset: isize, expected: &[u8], label: &str) -> bool {
    if sys_lseek(fd, offset, SEEK_SET) != offset {
        println!("  {}: lseek failed", label);
        return false;
    }
    let mut buf = [0u8; 64];
    if expected.len() > buf.len() {
        println!("  {}: test expectation exceeds scratch buffer", label);
        return false;
    }
    let n = sys_read(fd, &mut buf[..expected.len()]);
    if n != expected.len() as isize || &buf[..expected.len()] != expected {
        println!(
            "  {}: content mismatch, read={} expected={}",
            label,
            n,
            expected.len()
        );
        return false;
    }
    true
}

fn read_path_exact(path: &str, expected: &[u8], label: &str) -> bool {
    let fd = sys_open(path, O_RDONLY);
    if fd < 0 {
        println!("  {}: open failed: {}", label, fd);
        return false;
    }
    let ok = read_fd_exact(fd as usize, 0, expected, label);
    let close_ok = sys_close(fd as usize) == 0;
    ok && close_ok
}

fn path_absent(path: &str, label: &str) -> bool {
    let fd = sys_open(path, O_RDONLY);
    if fd >= 0 {
        println!("  {}: path unexpectedly exists", label);
        let _ = sys_close(fd as usize);
        false
    } else if fd != ENOENT {
        println!("  {}: expected ENOENT, got {}", label, fd);
        false
    } else {
        true
    }
}

fn test_nonempty_rmdir_preserves_contents() -> bool {
    if sys_mkdirat(AT_FDCWD, RMDIR_DIR, 0o755) < 0
        || !create_file_closed(RMDIR_CHILD, b"rmdir-child")
    {
        println!("  rmdir setup failed");
        return false;
    }

    let ret = sys_unlinkat(AT_FDCWD, RMDIR_DIR, AT_REMOVEDIR);
    let errno_ok = ret == ENOTEMPTY;
    if !errno_ok {
        println!("  non-empty rmdir expected {}, got {}", ENOTEMPTY, ret);
    }
    let content_ok = read_path_exact(RMDIR_CHILD, b"rmdir-child", "rmdir child preserved");
    errno_ok && content_ok
}

fn test_rename_same_inode_is_noop() -> bool {
    if !create_file_closed(SAME_SRC, b"same-inode") {
        return false;
    }
    let linked = sys_linkat(AT_FDCWD, SAME_SRC, AT_FDCWD, SAME_DST, 0);
    if linked < 0 {
        println!("  same-inode linkat failed: {}", linked);
        return false;
    }

    let ret = sys_renameat2(AT_FDCWD, SAME_SRC, AT_FDCWD, SAME_DST, 0);
    if ret != 0 {
        println!("  same-inode rename expected success, got {}", ret);
        return false;
    }
    let noop_ok = read_path_exact(SAME_SRC, b"same-inode", "same-inode source remains")
        && read_path_exact(SAME_DST, b"same-inode", "same-inode target remains");
    if !noop_ok {
        return false;
    }

    // The shared runtime state must remember both hard-link aliases. Removing
    // one known path cannot invalidate the already-looked-up surviving alias.
    let unlink = sys_unlinkat(AT_FDCWD, SAME_SRC, 0);
    unlink == 0
        && path_absent(SAME_SRC, "removed hardlink alias")
        && read_path_exact(SAME_DST, b"same-inode", "surviving hardlink alias")
}

fn test_hardlink_metadata_is_shared() -> bool {
    if !create_file_closed(META_SRC, b"shared-metadata") {
        return false;
    }
    let linked = sys_linkat(AT_FDCWD, META_SRC, AT_FDCWD, META_DST, 0);
    if linked != 0 {
        println!("  metadata hardlink setup failed: {}", linked);
        return false;
    }

    // Keep distinct path-derived VFS objects alive and populate both metadata
    // hot paths before changing either one.  A per-object cache would leave
    // META_DST stale after chmod through META_SRC.
    let src_fd = sys_open(META_SRC, O_RDONLY);
    let dst_fd = sys_open(META_DST, O_RDONLY);
    if src_fd < 0 || dst_fd < 0 {
        println!(
            "  metadata alias open failed: src={} dst={}",
            src_fd,
            dst_fd
        );
        if src_fd >= 0 {
            let _ = sys_close(src_fd as usize);
        }
        if dst_fd >= 0 {
            let _ = sys_close(dst_fd as usize);
        }
        return false;
    }
    let src_fd = src_fd as usize;
    let dst_fd = dst_fd as usize;
    let mut src_before = empty_stat();
    let mut dst_before = empty_stat();
    let primed = sys_fstat(src_fd, &mut src_before) == 0
        && sys_fstat(dst_fd, &mut dst_before) == 0
        && src_before.st_ino == dst_before.st_ino;
    if !primed {
        println!("  metadata alias stat priming failed");
        let _ = sys_close(src_fd);
        let _ = sys_close(dst_fd);
        return false;
    }

    let new_mode = if src_before.st_mode & 0o777 == 0o600 {
        0o640
    } else {
        0o600
    };
    let chmod = sys_fchmod_raw(src_fd, new_mode);
    let mut src_after = empty_stat();
    let mut dst_after = empty_stat();
    // No close/reopen or cache-cold operation is allowed between chmod and
    // the alias stat: the shared inode-state cache must publish immediately.
    let src_stat = sys_fstat(src_fd, &mut src_after);
    let dst_stat = sys_fstat(dst_fd, &mut dst_after);
    let close_src = sys_close(src_fd);
    let close_dst = sys_close(dst_fd);
    let expected = new_mode as u32;
    let coherent = chmod == 0
        && src_stat == 0
        && dst_stat == 0
        && src_after.st_ino == dst_after.st_ino
        && src_after.st_mode & 0o777 == expected
        && dst_after.st_mode & 0o777 == expected
        && close_src == 0
        && close_dst == 0;
    if !coherent {
        println!(
            "  hardlink metadata incoherent: chmod={} src_stat={} dst_stat={} src_mode={:o} dst_mode={:o}",
            chmod,
            src_stat,
            dst_stat,
            src_after.st_mode & 0o777,
            dst_after.st_mode & 0o777
        );
    }
    coherent
}

fn test_failed_rename_preserves_both_sides() -> bool {
    if !create_file_closed(NR_SRC, b"noreplace-source")
        || !create_file_closed(NR_DST, b"noreplace-target")
    {
        return false;
    }
    let ret = sys_renameat2(
        AT_FDCWD,
        NR_SRC,
        AT_FDCWD,
        NR_DST,
        RENAME_NOREPLACE,
    );
    let noreplace_ok = ret == EEXIST;
    if !noreplace_ok {
        println!("  RENAME_NOREPLACE expected {}, got {}", EEXIST, ret);
    }
    let files_ok = read_path_exact(NR_SRC, b"noreplace-source", "noreplace source")
        && read_path_exact(NR_DST, b"noreplace-target", "noreplace target");

    let src_mkdir = sys_mkdirat(AT_FDCWD, DIR_SRC, 0o755);
    let dst_mkdir = sys_mkdirat(AT_FDCWD, DIR_DST, 0o755);
    let dir_setup_ok = src_mkdir == 0
        && dst_mkdir == 0
        && create_file_closed(DIR_DST_CHILD, b"nonempty-target");
    if !dir_setup_ok {
        println!(
            "  directory rename setup failed: src={} dst={}",
            src_mkdir,
            dst_mkdir
        );
        return false;
    }
    let dir_ret = sys_renameat2(AT_FDCWD, DIR_SRC, AT_FDCWD, DIR_DST, 0);
    let dir_errno_ok = dir_ret == ENOTEMPTY;
    if !dir_errno_ok {
        println!("  non-empty target rename expected {}, got {}", ENOTEMPTY, dir_ret);
    }
    let mut src_stat = empty_stat();
    let src_stat_ret = sys_fstatat(AT_FDCWD, DIR_SRC, &mut src_stat, 0);
    let src_exists = src_stat_ret == 0;
    if !src_exists {
        println!("  failed rename removed source directory: {}", src_stat_ret);
    }
    let target_ok = read_path_exact(DIR_DST_CHILD, b"nonempty-target", "failed rename target");

    noreplace_ok && files_ok && dir_errno_ok && src_exists && target_ok
}

fn test_empty_directory_overwrite_fails_closed() -> bool {
    if sys_mkdirat(AT_FDCWD, EMPTY_SRC, 0o755) != 0
        || sys_mkdirat(AT_FDCWD, EMPTY_DST, 0o755) != 0
    {
        println!("  empty-directory overwrite setup failed");
        return false;
    }
    let ret = sys_renameat2(AT_FDCWD, EMPTY_SRC, AT_FDCWD, EMPTY_DST, 0);
    if ret != EOPNOTSUPP {
        println!(
            "  empty-directory overwrite expected {}, got {}",
            EOPNOTSUPP,
            ret
        );
        return false;
    }
    let mut src = empty_stat();
    let mut dst = empty_stat();
    let preserved = sys_fstatat(AT_FDCWD, EMPTY_SRC, &mut src, 0) == 0
        && sys_fstatat(AT_FDCWD, EMPTY_DST, &mut dst, 0) == 0;
    if !preserved {
        println!("  fail-closed directory overwrite changed a namespace");
    }
    preserved
}

fn test_open_directory_fd_follows_rename() -> bool {
    if sys_mkdirat(AT_FDCWD, DIRFD_OLD, 0o755) != 0 {
        println!("  dirfd rename setup failed");
        return false;
    }
    let dirfd = sys_open(DIRFD_OLD, O_RDONLY);
    if dirfd < 0 {
        println!("  opening source directory failed: {}", dirfd);
        return false;
    }
    let dirfd = dirfd as usize;
    let renamed = sys_renameat2(AT_FDCWD, DIRFD_OLD, AT_FDCWD, DIRFD_NEW, 0);
    if renamed != 0 {
        println!("  directory rename failed: {}", renamed);
        let _ = sys_close(dirfd);
        return false;
    }

    let child_fd = sys_openat(
        dirfd as isize,
        DIRFD_CHILD_NAME.as_ptr(),
        O_CREAT | O_RDWR | O_TRUNC,
        0o644,
    );
    if child_fd < 0 {
        println!("  openat through renamed directory fd failed: {}", child_fd);
        let _ = sys_close(dirfd);
        return false;
    }
    let child_fd = child_fd as usize;
    let wrote = sys_write(child_fd, b"dirfd-live");
    let synced = sys_fsync(child_fd);
    let child_close = sys_close(child_fd);
    let dir_close = sys_close(dirfd);
    wrote == 10
        && synced == 0
        && child_close == 0
        && dir_close == 0
        && path_absent(DIRFD_OLD, "renamed directory old path")
        && read_path_exact(DIRFD_CHILD, b"dirfd-live", "renamed directory fd child")
}

fn test_overwrite_keeps_open_target_inode() -> bool {
    if !create_file_closed(OVER_SRC, b"new-source") {
        return false;
    }
    let old_fd = match create_file(OVER_DST, b"old-target") {
        Ok(fd) => fd,
        Err(err) => {
            println!("  overwrite target setup failed: {}", err);
            return false;
        }
    };

    let ret = sys_renameat2(AT_FDCWD, OVER_SRC, AT_FDCWD, OVER_DST, 0);
    if ret != 0 {
        println!("  overwrite rename failed: {}", ret);
        let _ = sys_close(old_fd);
        return false;
    }

    let old_before = read_fd_exact(old_fd, 0, b"old-target", "open overwritten target");
    let seek = sys_lseek(old_fd, 0, SEEK_END);
    let wrote = sys_write(old_fd, b"-fd");
    let sync = sys_fsync(old_fd);
    let old_after = read_fd_exact(old_fd, 0, b"old-target-fd", "mutated unlinked target");
    let close = sys_close(old_fd);
    if seek != 10 || wrote != 3 || sync != 0 || close != 0 {
        println!(
            "  overwritten target fd mutation failed: seek={} write={} fsync={} close={}",
            seek,
            wrote,
            sync,
            close
        );
        return false;
    }

    old_before
        && old_after
        && path_absent(OVER_SRC, "overwrite source removed")
        && read_path_exact(OVER_DST, b"new-source", "overwrite publishes source")
}

fn test_open_source_fd_follows_rename() -> bool {
    let fd = match create_file(MOVE_SRC, b"moved") {
        Ok(fd) => fd,
        Err(err) => {
            println!("  move source setup failed: {}", err);
            return false;
        }
    };
    let ret = sys_renameat2(AT_FDCWD, MOVE_SRC, AT_FDCWD, MOVE_DST, 0);
    if ret != 0 {
        println!("  move rename failed: {}", ret);
        let _ = sys_close(fd);
        return false;
    }

    let seek = sys_lseek(fd, 0, SEEK_END);
    let wrote = sys_write(fd, b"-fd");
    let sync = sys_fsync(fd);
    let via_fd = read_fd_exact(fd, 0, b"moved-fd", "renamed source fd");
    let close = sys_close(fd);
    if seek != 5 || wrote != 3 || sync != 0 || close != 0 {
        println!(
            "  renamed source fd mutation failed: seek={} write={} fsync={} close={}",
            seek,
            wrote,
            sync,
            close
        );
        return false;
    }

    via_fd
        && path_absent(MOVE_SRC, "old source path remains absent")
        && read_path_exact(MOVE_DST, b"moved-fd", "renamed destination follows inode")
}

fn test_unlink_open_name_reuse_isolated() -> bool {
    let old_fd = match create_file(UNLINK_PATH, b"old-open") {
        Ok(fd) => fd,
        Err(err) => {
            println!("  unlink setup failed: {}", err);
            return false;
        }
    };
    let mut old_stat = empty_stat();
    if sys_fstat(old_fd, &mut old_stat) != 0 {
        println!("  fstat old unlinked candidate failed");
        let _ = sys_close(old_fd);
        return false;
    }

    let unlink = sys_unlinkat(AT_FDCWD, UNLINK_PATH, 0);
    let absent_before_reuse = path_absent(UNLINK_PATH, "unlinked name");
    if unlink != 0 || !absent_before_reuse {
        println!("  unlink while open failed: {}", unlink);
        let _ = sys_close(old_fd);
        return false;
    }

    let new_fd = match create_file(UNLINK_PATH, b"new-name") {
        Ok(fd) => fd,
        Err(err) => {
            println!("  recreate unlinked name failed: {}", err);
            let _ = sys_close(old_fd);
            return false;
        }
    };
    let mut new_stat = empty_stat();
    let new_stat_ok = sys_fstat(new_fd, &mut new_stat) == 0;
    let new_close = sys_close(new_fd);
    if !new_stat_ok || new_close != 0 {
        println!("  recreated file stat/close failed");
        let _ = sys_close(old_fd);
        return false;
    }
    let distinct_inode = old_stat.st_ino != new_stat.st_ino;
    if !distinct_inode {
        println!(
            "  live unlinked inode was reused: old={} new={}",
            old_stat.st_ino,
            new_stat.st_ino
        );
    }

    let seek = sys_lseek(old_fd, 0, SEEK_END);
    let wrote = sys_write(old_fd, b"-fd");
    let sync = sys_fsync(old_fd);
    let old_ok = read_fd_exact(old_fd, 0, b"old-open-fd", "old unlinked fd");
    let replacement_before_close =
        read_path_exact(UNLINK_PATH, b"new-name", "replacement before old close");
    let old_close = sys_close(old_fd);
    let replacement_after_close =
        read_path_exact(UNLINK_PATH, b"new-name", "replacement after old close");
    if seek != 8 || wrote != 3 || sync != 0 || old_close != 0 {
        println!(
            "  unlinked fd mutation failed: seek={} write={} fsync={} close={}",
            seek,
            wrote,
            sync,
            old_close
        );
        return false;
    }

    distinct_inode && old_ok && replacement_before_close && replacement_after_close
}

pub fn run() -> i32 {
    println!("[regression_lwext4_namespace] start");
    cleanup_fixture();
    if sys_mkdirat(AT_FDCWD, BASE, 0o755) != 0 {
        println!("FAIL: cannot create lwext4 namespace fixture");
        return 1;
    }

    let cases: &[(&str, fn() -> bool)] = &[
        ("nonempty_rmdir_preserves_contents", test_nonempty_rmdir_preserves_contents),
        ("rename_same_inode_is_noop", test_rename_same_inode_is_noop),
        ("hardlink_metadata_is_shared", test_hardlink_metadata_is_shared),
        ("failed_rename_preserves_both_sides", test_failed_rename_preserves_both_sides),
        ("empty_directory_overwrite_fails_closed", test_empty_directory_overwrite_fails_closed),
        ("open_directory_fd_follows_rename", test_open_directory_fd_follows_rename),
        ("overwrite_keeps_open_target_inode", test_overwrite_keeps_open_target_inode),
        ("open_source_fd_follows_rename", test_open_source_fd_follows_rename),
        ("unlink_open_name_reuse_isolated", test_unlink_open_name_reuse_isolated),
    ];

    let mut failed = 0usize;
    for (name, case) in cases {
        if case() {
            println!("  PASS: {}", name);
        } else {
            failed += 1;
            println!("  FAIL: {}", name);
        }
    }
    cleanup_fixture();

    if failed == 0 {
        println!("[regression_lwext4_namespace] PASS");
        0
    } else {
        println!("[regression_lwext4_namespace] FAIL: {} case(s)", failed);
        1
    }
}
