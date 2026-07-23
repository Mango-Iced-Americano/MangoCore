use crate::runner::process::run_bash_cmd;
use user_lib::println;
pub fn prepare_layout(environ: &[*const u8]) {
    println!("[initproc] ensuring base directories...");
    let _ = run_bash_cmd("mkdir -p /bin /lib /usr /root /tmp /run /var /var/tmp /dev/shm /glibc/lib; chmod 1777 /tmp /var/tmp /dev/shm; test -e /bin/busybox || ln -s /busybox /bin/busybox; test -f /bin/ls || /bin/busybox --install -s /bin 2>/dev/null || true; test -e /bin/bash || ln -s /bash /bin/bash; [ -e /bin/sh ] || ln -sf /bin/bash /bin/sh; test -f /bin/sh || true; chmod +x /musl/*.sh /musl/*/*.sh /glibc/*.sh /glibc/*/*.sh 2>/dev/null; true\0", environ);
    println!("[initproc] runtime layout ready");
}
