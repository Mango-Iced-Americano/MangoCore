use crate::runner::process::run_bash_cmd;
use user_lib::println;
pub fn install_apk_packages(environ: &[*const u8], skip: bool) -> bool {
    if skip {
        println!("[initproc] skip_apk=true: skipping install_apk_packages");
        return true;
    }
    if run_bash_cmd("test -x /sbin/mkfs.ext4\0", environ) == 0
        && run_bash_cmd("test -f /lib/apk/db/installed\0", environ) == 0
        && run_bash_cmd("grep -q e2fsprogs /lib/apk/db/installed\0", environ) == 0
    {
        println!("[initproc] apk packages already installed (verified), skipping");
        return true;
    }
    if run_bash_cmd(
        "mkdir -p /lib/apk/db /var/cache/apk /etc/apk; [ -f /etc/apk/world ] || : > /etc/apk/world\0",
        environ,
    ) != 0
    {
        println!("[initproc] ERROR: failed to create APK directories");
        return false;
    }
    if run_bash_cmd("test -s /etc/apk/repositories\0", environ) != 0 {
        println!("[initproc] WARNING: /etc/apk/repositories missing or empty — APK may fail");
    }
    if run_bash_cmd("test -d /etc/apk/keys\0", environ) != 0 {
        println!("[initproc] WARNING: /etc/apk/keys missing — APK may fail");
    }
    println!("[initproc] apk add e2fsprogs ...");
    if run_bash_cmd("/tools/bin/apk.static add e2fsprogs\0", environ) != 0 {
        println!("[initproc] ERROR: apk add e2fsprogs failed");
        return false;
    }
    let _ = run_bash_cmd(
        "ln -sf /sbin/mkfs.ext4 /bin/mkfs.ext4 2>/dev/null; ln -sf /sbin/mke2fs /bin/mke2fs 2>/dev/null\0",
        environ,
    );
    if run_bash_cmd("test -x /sbin/mkfs.ext4\0", environ) != 0 {
        println!("[initproc] ERROR: /sbin/mkfs.ext4 not found after APK install");
        return false;
    }
    println!("[initproc] apk packages installed successfully");
    true
}
