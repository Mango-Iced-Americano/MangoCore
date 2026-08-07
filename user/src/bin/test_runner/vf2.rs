//! VF2 / StarFive VisionFive 2 board SD-root chroot logic (moved from
//! `init/vf2.rs`).
//!
//! The test-runner decides by mode whether to chroot into the SD root:
//! - `Shell` mode on a real VF2 board: probe /proc/device-tree/model, mount the
//!   SD root at /newroot, bind pseudo-filesystems in, then chroot + exec
//!   /sbin/init. This never returns on the board.
//! - QEMU (model not StarFive) or `Run` mode: `try_boot()` returns immediately
//!   and the test-runner stays in the initramfs with the SD card at /sdcard.

use alloc::string::String;
use alloc::vec::Vec;
use super::vf2_mounts;
use user_lib::syscall::sys_open;
use user_lib::{chdir, chroot, close, exec, getdents64, open, println, read, OpenFlags};

const NEW_ROOT: &str = "/newroot\0";
const ROOT_INIT: &str = "/sbin/init\0";
const ROOT_SHELL: &str = "/bin/sh\0";
const RESCUE_SHELL: &str = "/rescue/sh\0";
const O_DIRECTORY: u32 = 0o200000;
const DT_BLK: u8 = 6;
const DIRENT64_HEADER_LEN: usize = 19;
const MAX_ROOT_CANDIDATES: usize = 8;

struct DeviceCandidate {
    name: String,
    rank: usize,
}

/// Probe the board and chroot into the SD root when this is a VisionFive.
/// Returns normally (continuing the initramfs) on QEMU or when no SD root is
/// available.
pub(crate) fn try_boot() {
    let mut model = [0u8; 256];
    let Some(model) = platform_model(&mut model) else {
        return;
    };
    if !is_visionfive_model(model) {
        return;
    }

    println!("[test-runner] VF2 model read: {}", model);
    println!("[test-runner] VF2 probing /dev for SD root");
    let Some(device) = mount_root_device() else {
        println!("[test-runner] VF2 SD root unavailable; continuing initramfs");
        return;
    };
    println!(
        "[test-runner] VF2 mounted {} at /newroot",
        device.trim_end_matches('\0')
    );
    println!("[test-runner] VF2 bind-mounting pseudo-filesystems into /newroot");
    if !vf2_mounts::bind_pseudo_filesystems_in(NEW_ROOT) {
        println!("[test-runner] VF2 pseudo-filesystem bind failed; continuing initramfs");
        return;
    }
    chroot_to(NEW_ROOT);
}

fn platform_model<'a>(buffer: &'a mut [u8]) -> Option<&'a str> {
    let fd = open("/proc/device-tree/model\0", OpenFlags::RDONLY);
    if fd < 0 {
        return None;
    }
    let size = read(fd as usize, buffer);
    let _ = user_lib::close(fd as usize);
    if size <= 0 {
        return None;
    }
    core::str::from_utf8(&buffer[..size as usize]).ok()
}

fn is_visionfive_model(model: &str) -> bool {
    let mut has_starfive = false;
    let mut has_visionfive = false;
    for part in model.split(|character: char| !character.is_ascii_alphanumeric()) {
        has_starfive |= part.eq_ignore_ascii_case("starfive");
        has_visionfive |= part.eq_ignore_ascii_case("visionfive");
    }
    has_starfive && has_visionfive
}

fn mount_root_device() -> Option<String> {
    for candidate in discover_root_candidates() {
        let device = alloc::format!("/dev/{}\0", candidate.name);
        println!(
            "[test-runner] VF2 trying root device {}",
            device.trim_end_matches('\0')
        );
        if vf2_mounts::mount_root_filesystem(&device, NEW_ROOT) {
            return Some(device);
        }
    }
    None
}

fn discover_root_candidates() -> Vec<DeviceCandidate> {
    let fd = sys_open("/dev\0", OpenFlags::RDONLY.bits() | O_DIRECTORY);
    if fd < 0 {
        println!("[test-runner] VF2 open /dev failed: {}", fd);
        return Vec::new();
    }

    let mut candidates = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let count = getdents64(fd as usize, &mut buffer);
        if count < 0 {
            println!("[test-runner] VF2 getdents64 /dev failed: {}", count);
            break;
        }
        if count == 0 {
            break;
        }
        collect_root_candidates(&buffer[..count as usize], &mut candidates);
    }
    let _ = close(fd as usize);

    candidates.sort_by_key(|candidate| candidate.rank);
    candidates.truncate(MAX_ROOT_CANDIDATES);
    candidates
}

fn collect_root_candidates(entries: &[u8], candidates: &mut Vec<DeviceCandidate>) {
    let mut offset = 0usize;
    while offset + DIRENT64_HEADER_LEN <= entries.len() {
        let record_len = u16::from_ne_bytes([entries[offset + 16], entries[offset + 17]]) as usize;
        if record_len < DIRENT64_HEADER_LEN || offset + record_len > entries.len() {
            break;
        }

        let name_start = offset + DIRENT64_HEADER_LEN;
        let name_end = entries[name_start..offset + record_len]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(offset + record_len, |length| name_start + length);
        if let Ok(name) = core::str::from_utf8(&entries[name_start..name_end]) {
            if let Some(rank) = device_rank(name, entries[offset + 18]) {
                if !candidates.iter().any(|candidate| candidate.name == name) {
                    candidates.push(DeviceCandidate {
                        name: String::from(name),
                        rank,
                    });
                }
            }
        }
        offset += record_len;
    }
}

fn device_rank(name: &str, file_type: u8) -> Option<usize> {
    if name == "mmcblk0p1" {
        Some(0)
    } else if name.starts_with("mmcblk0p") {
        Some(1)
    } else if name.starts_with("mmcblk") {
        Some(2)
    } else if name.starts_with("vd") {
        Some(3)
    } else if file_type == DT_BLK && !name.starts_with("ram") {
        Some(4)
    } else {
        None
    }
}

/// Fork a rescue shell loop that keeps respawning /rescue/sh forever. Used as
/// the safety net on all VF2 failure paths.
pub(crate) fn rescue_forever() -> ! {
    use user_lib::{exit, fork, sleep, waitpid_wnohang};
    println!("[test-runner] entering rescue shell");
    loop {
        let shell = fork();
        if shell == 0 {
            exec(
                RESCUE_SHELL,
                &[RESCUE_SHELL.as_ptr(), core::ptr::null()],
                &[core::ptr::null()],
            );
            exit(127);
        }
        if shell < 0 {
            let mut status = 0;
            while waitpid_wnohang(-1, &mut status) > 0 {}
            sleep(100);
            continue;
        }
        let mut status = 0;
        while waitpid_wnohang(shell, &mut status) == 0 {
            let mut reap = 0;
            while waitpid_wnohang(-1, &mut reap) > 0 {}
            sleep(100);
        }
    }
}

fn chroot_to(root: &str) -> ! {
    let ret = chdir("/\0");
    if ret < 0 {
        println!("[test-runner] VF2 chdir before chroot failed: {}", ret);
        rescue_forever();
    }
    let ret = chroot(root);
    if ret < 0 {
        println!(
            "[test-runner] VF2 chroot {} failed: {}",
            root.trim_end_matches('\0'),
            ret
        );
        rescue_forever();
    }
    let ret = chdir("/\0");
    if ret < 0 {
        println!("[test-runner] VF2 chdir after chroot failed: {}", ret);
        rescue_forever();
    }

    println!("[test-runner] VF2 chroot complete; exec /sbin/init");
    let ret = exec(
        ROOT_INIT,
        &[ROOT_INIT.as_ptr(), core::ptr::null()],
        &[core::ptr::null()],
    );
    println!("[test-runner] VF2 exec /sbin/init failed: {}; trying /bin/sh", ret);
    let ret = exec(
        ROOT_SHELL,
        &[ROOT_SHELL.as_ptr(), core::ptr::null()],
        &[core::ptr::null()],
    );
    println!("[test-runner] VF2 exec /bin/sh failed: {}; entering rescue shell", ret);
    rescue_forever();
}
