use alloc::format;
use crate::runner::process::run_bash_cmd;
use user_lib::println;
pub fn install_apk_packages(environ: &[*const u8], skip: bool) {
    if skip { println!("[initproc] skip_apk=true: skipping install_apk_packages"); return; }
    let command = format!("/tools/bin/apk.static add e2fsprogs && rm -f /bin/mkfs.ext2 /bin/mkfs.ext3 /bin/mkfs.ext4 /bin/mke2fs\0");
    println!("[initproc] apk add e2fsprogs ..."); let _ = run_bash_cmd(&command, environ);
}
