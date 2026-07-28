use crate::runner::process::run_bash_cmd;
use user_lib::println;
pub fn prepare_layout(environ: &[*const u8]) {
    println!("[initproc] ensuring base directories...");
    let _ = run_bash_cmd("/busybox mkdir -p /bin; /busybox --install -s /bin; export PATH=/bin; mkdir -p /lib /usr /root /tmp /run /var /var/tmp /dev/shm; if [ -d /sdcard/musl ] && [ ! -L /musl ]; then rm -f /musl/.gitkeep; (rmdir /musl 2>/dev/null || true); ln -sf /sdcard/musl /musl; fi; if [ -d /sdcard/glibc ] && [ ! -L /glibc ]; then rm -f /glibc/.gitkeep; (rmdir /glibc 2>/dev/null || true); ln -sf /sdcard/glibc /glibc; fi; mkdir -p /glibc/lib; chmod 1777 /tmp /var/tmp /dev/shm; test -e /bin/bash || ln -s /bin/sh /bin/bash; chmod +x /musl/*.sh /musl/*/*.sh /glibc/*.sh /glibc/*/*.sh 2>/dev/null; true\0", environ);
    println!("[initproc] runtime layout ready");
}
