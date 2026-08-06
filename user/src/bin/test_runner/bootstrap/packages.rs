use alloc::format;
use crate::runner::process::run_bash_cmd;
use user_lib::println;

pub fn install_apk_packages(environ: &[*const u8], skip: bool) {
    if skip {
        println!("[initproc] skip_apk=true: skipping install_apk_packages");
        return;
    }
    if run_bash_cmd("test -f /bin/mkfs.ext4\0", environ) == 0 {
        println!("[initproc] apk packages already installed, skipping");
        return;
    }
    // 工具盘已经预装 e2fsprogs 时直接复用，避免每次测试启动都通过 APK 重装。
    if run_bash_cmd("test -x /tools/sbin/mkfs.ext4\0", environ) == 0
        && run_bash_cmd("/sbin/mkfs.ext4 -V\0", environ) == 0
    {
        println!("[initproc] using preinstalled e2fsprogs from /tools");
        return;
    }
    // APK 3.0.6 needs /lib/apk/db (database) and /var/cache/apk (cache).
    // These were created by the old stage-1 init.rs, removed during PID1 refactor.
    let _ = run_bash_cmd("mkdir -p /lib/apk/db /var/cache/apk /etc/apk; [ -f /etc/apk/world ] || : > /etc/apk/world; test -s /etc/apk/repositories; test -d /etc/apk/keys\0", environ);
    let command = format!("/tools/bin/apk.static add e2fsprogs && rm -f /bin/mkfs.ext2 /bin/mkfs.ext3 /bin/mkfs.ext4 /bin/mke2fs\0");
    println!("[initproc] apk add e2fsprogs ...");
    let _ = run_bash_cmd(&command, environ);
}
