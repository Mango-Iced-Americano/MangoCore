use alloc::format;
use user_lib::syscall::{sys_mkdirat, sys_mount};
use user_lib::{mount, println};

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

fn try_mount(source: &'static str, target: &'static str, fstype: &'static str) -> bool {
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
    let _ = try_mount("none\0", "/proc\0", "proc\0");
    let _ = try_mount("none\0", "/sys\0", "sysfs\0");
    let _ = try_mount("none\0", "/run\0", "tmpfs\0");
    let _ = try_mount("none\0", "/dev/shm\0", "tmpfs\0");
}

pub(super) fn mount_tmpfs(target: &'static str) {
    let _ = try_mount("none\0", target, "tmpfs\0");
}

fn try_bind_mount(source: &str, target: &str) {
    let src = format!("{}\0", source);
    let tgt = format!("{}\0", target);
    let ret = mount(src.as_ptr(), tgt.as_ptr(), "\0".as_ptr(), MS_BIND, 0);
    if ret == 0 {
        println!("[init] bind mount {} -> {}", source, target);
    } else {
        println!("[init] bind mount {} -> {}: skipped (errno={})", source, target, -ret);
    }
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
        try_bind_mount(source, target);
    }
}
