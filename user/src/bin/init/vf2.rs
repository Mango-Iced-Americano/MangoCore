use super::mounts;
use user_lib::{chdir, chroot, exec, open, println, read, OpenFlags};

const SDCARD_ROOT: &str = "/sdcard\0";
const SDCARD_INIT: &str = "/sbin/init\0";
const SDCARD_SHELL: &str = "/bin/sh\0";

pub(super) fn try_boot() {
    let mut model = [0u8; 256];
    let Some(model) = platform_model(&mut model) else {
        return;
    };
    if !is_visionfive_model(model) {
        return;
    }

    println!("[init] VF2 model read: {}", model);
    if !mounts::sdcard_root_ready() {
        println!("[init] VF2 SD root unavailable; continuing initramfs");
        return;
    }
    println!("[init] VF2 bind-mounting pseudo-filesystems into /sdcard");
    if !mounts::bind_pseudo_filesystems_in_sdcard() {
        println!("[init] VF2 pseudo-filesystem bind failed; continuing initramfs");
        return;
    }
    chroot_to_sdcard();
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

fn chroot_to_sdcard() -> ! {
    let ret = chdir("/\0");
    if ret < 0 {
        println!("[init] VF2 chdir before chroot failed: {}", ret);
        super::rescue_forever();
    }
    let ret = chroot(SDCARD_ROOT);
    if ret < 0 {
        println!("[init] VF2 chroot /sdcard failed: {}", ret);
        super::rescue_forever();
    }
    let ret = chdir("/\0");
    if ret < 0 {
        println!("[init] VF2 chdir after chroot failed: {}", ret);
        super::rescue_forever();
    }

    println!("[init] VF2 chroot complete; exec /sbin/init");
    let ret = exec(
        SDCARD_INIT,
        &[SDCARD_INIT.as_ptr(), core::ptr::null()],
        &[core::ptr::null()],
    );
    println!("[init] VF2 exec /sbin/init failed: {}; trying /bin/sh", ret);
    let ret = exec(
        SDCARD_SHELL,
        &[SDCARD_SHELL.as_ptr(), core::ptr::null()],
        &[core::ptr::null()],
    );
    println!("[init] VF2 exec /bin/sh failed: {}; entering rescue shell", ret);
    super::rescue_forever();
}
