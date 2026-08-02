use alloc::format;
use user_lib::syscall::{sys_faccessat2, sys_mkdirat, sys_mount, sys_umount2};
use user_lib::{chmod, mount, println};

const AT_FDCWD: isize = -100;
const MS_BIND: usize = 4096;

pub(super) fn prepare_pseudo_fs_framework() {
    for path in [
        "/dev\0",
        "/proc\0",
        "/sys\0",
        "/run\0",
        "/tmp\0",
        "/dev/shm\0",
    ] {
        let _ = sys_mkdirat(AT_FDCWD, path, 0o755);
    }
    println!("[init] pseudo-fs mount framework ready");
}

fn try_mount(source: &str, target: &str, fstype: &str) -> bool {
    let result = mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), 0, 0);
    if result < 0 {
        println!(
            "[init] mount {} at {} failed: {}",
            fstype.trim_end_matches('\0'),
            target.trim_end_matches('\0'),
            result
        );
        return false;
    }
    true
}

pub(super) fn mount_pseudo_filesystems() {
    if try_mount("none\0", "/dev\0", "devtmpfs\0") {
        // Ensure the devtmpfs cover directory exists before mounting its tmpfs child.
        let _ = sys_mkdirat(AT_FDCWD, "/dev/shm\0", 0o1777);
        let _ = try_mount("none\0", "/dev/shm\0", "tmpfs\0");        let _ = chmod("/dev/shm\0", 0o1777);
    }
    let _ = try_mount("none\0", "/proc\0", "proc\0");
    let _ = try_mount("none\0", "/sys\0", "sysfs\0");
    let _ = try_mount("none\0", "/run\0", "tmpfs\0");
}

pub(super) fn mount_tmpfs(target: &'static str) {
    let _ = try_mount("none\0", target, "tmpfs\0");
}

fn try_bind_mount(source: &str, target: &str) -> bool {
    let src = format!("{}\0", source);
    let target = target.trim_end_matches('\0');
    let tgt = format!("{}\0", target);
    let ret = mount(src.as_ptr(), tgt.as_ptr(), "\0".as_ptr(), MS_BIND, 0);
    if ret == 0 {
        println!("[init] bind mount {} -> {}", source, target);
        true
    } else {
        println!("[init] bind mount {} -> {}: skipped (errno={})", source, target, -ret);
        false
    }
}

pub(super) fn mount_root_filesystem(source: &str, root: &str) -> bool {
    let _ = sys_mkdirat(AT_FDCWD, root, 0o755);
    for fstype in ["ext4\0", "vfat\0", "fat32\0"] {
        if !try_mount(source, root, fstype) {
            continue;
        }
        if root_looks_ready(root) {
            println!(
                "[init] VF2 root {} mounted as {}",
                source.trim_end_matches('\0'),
                fstype.trim_end_matches('\0')
            );
            return true;
        }

        println!(
            "[init] VF2 {} is not a root filesystem; unmounting {}",
            source.trim_end_matches('\0'),
            root.trim_end_matches('\0')
        );
        let unmount = sys_umount2(root.as_ptr(), 0);
        if unmount < 0 {
            println!(
                "[init] VF2 unmount {} failed: {}",
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

pub(super) fn bind_pseudo_filesystems_in(root: &str) -> bool {
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

/// Bind persistent directories after the kernel has mounted its boot devices.
pub(super) fn setup_persistent_mounts() {
    // /tmp: prefer ext4-backed storage when kernel x0 mounting succeeded.
    let _ = sys_mkdirat(AT_FDCWD, "/sdcard/tmp\0", 0o1777);
    let tmp_result = sys_mount(
        "/sdcard/tmp\0".as_ptr(),
        "/tmp\0".as_ptr(),
        core::ptr::null(),
        MS_BIND,
        0,
    );
    if tmp_result < 0 {
        println!("[init] bind-mount /sdcard/tmp → /tmp failed: {}, falling back to tmpfs", tmp_result);
        mount_tmpfs("/tmp\0");
    } else {
        println!("[init] /tmp is bind-mounted from ext4 /sdcard/tmp");
    }

    let etc_result = sys_mount(
        "/tools/etc\0".as_ptr(),
        "/etc\0".as_ptr(),
        core::ptr::null(),
        MS_BIND,
        0,
    );
    if etc_result < 0 {
        println!("[init] bind-mount /tools/etc → /etc failed: {}, keeping initramfs /etc", etc_result);
    } else {
        println!("[init] /etc is bind-mounted from tools disk");
    }

    for (source, target) in [
        ("/tools/bin", "/bin"),
        ("/tools/sbin", "/sbin"),
        ("/tools/lib", "/lib"),
        ("/tools/usr", "/usr"),
        ("/tools/root", "/root"),
    ] {
        let tgt_path = format!("{}\0", target);
        let _ = sys_mkdirat(AT_FDCWD, &tgt_path, 0o755);
        let _ = try_bind_mount(source, target);
    }
}
