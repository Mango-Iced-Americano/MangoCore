//! Mount helpers for the VF2 SD-root chroot path (moved from `init/mounts.rs`).
//!
//! The test-runner owns the decision whether to chroot into the SD root (Shell
//! mode on a real board) or run tests inside the initramfs (Run mode, SD card
//! accessible at /sdcard). These helpers bind pseudo-filesystems into the
//! candidate root and validate it looks like a root filesystem.

use alloc::format;
use user_lib::syscall::{sys_faccessat2, sys_mkdirat, sys_umount2};
use user_lib::{mount, println};

const AT_FDCWD: isize = -100;
const MS_BIND: usize = 4096;

fn try_mount(source: &str, target: &str, fstype: &str) -> bool {
    let result = mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), 0, 0);
    if result < 0 {
        println!(
            "[test-runner] mount {} at {} failed: {}",
            fstype.trim_end_matches('\0'),
            target.trim_end_matches('\0'),
            result
        );
        return false;
    }
    true
}

fn try_bind_mount(source: &str, target: &str) -> bool {
    let src = format!("{}\0", source);
    let target = target.trim_end_matches('\0');
    let tgt = format!("{}\0", target);
    let ret = mount(src.as_ptr(), tgt.as_ptr(), "\0".as_ptr(), MS_BIND, 0);
    if ret == 0 {
        println!("[test-runner] bind mount {} -> {}", source, target);
        true
    } else {
        println!(
            "[test-runner] bind mount {} -> {}: skipped (errno={})",
            source,
            target,
            -ret
        );
        false
    }
}

/// Try to mount `source` (e.g. `/dev/mmcblk0p1`) at `root` as a real root
/// filesystem. Returns true only if a mount succeeded and the directory looks
/// like a root (has /bin or /etc).
pub(crate) fn mount_root_filesystem(source: &str, root: &str) -> bool {
    let _ = sys_mkdirat(AT_FDCWD, root, 0o755);
    for fstype in ["ext4\0", "vfat\0", "fat32\0"] {
        if !try_mount(source, root, fstype) {
            continue;
        }
        if root_looks_ready(root) {
            println!(
                "[test-runner] VF2 root {} mounted as {}",
                source.trim_end_matches('\0'),
                fstype.trim_end_matches('\0')
            );
            return true;
        }

        println!(
            "[test-runner] VF2 {} is not a root filesystem; unmounting {}",
            source.trim_end_matches('\0'),
            root.trim_end_matches('\0')
        );
        let unmount = sys_umount2(root.as_ptr(), 0);
        if unmount < 0 {
            println!(
                "[test-runner] VF2 unmount {} failed: {}",
                root.trim_end_matches('\0'),
                unmount
            );
            return false;
        }
    }
    false
}

fn root_looks_ready(root: &str) -> bool {
    let bin = root_path(root, "/bin");
    let etc = root_path(root, "/etc");
    sys_faccessat2(AT_FDCWD, &bin, 0, 0) == 0
        || sys_faccessat2(AT_FDCWD, &etc, 0, 0) == 0
}

fn root_path(root: &str, suffix: &str) -> alloc::string::String {
    format!("{}{}\0", root.trim_end_matches('\0'), suffix)
}

/// Bind /proc, /sys, /dev, /dev/shm, /run and /tmp into `root` so the chrooted
/// SD system has working pseudo-filesystems. Returns true when every bind
/// succeeded.
pub(crate) fn bind_pseudo_filesystems_in(root: &str) -> bool {
    for suffix in ["/proc", "/sys", "/dev", "/dev/shm", "/run", "/tmp"] {
        let target = root_path(root, suffix);
        let _ = sys_mkdirat(AT_FDCWD, &target, 0o755);
    }

    let mut mounted = true;
    for (source, suffix) in [
        ("/proc", "/proc"),
        ("/sys", "/sys"),
        ("/dev", "/dev"),
        ("/dev/shm", "/dev/shm"),
        ("/run", "/run"),
        ("/tmp", "/tmp"),
    ] {
        let target = root_path(root, suffix);
        mounted &= try_bind_mount(source, &target);
    }
    mounted
}
