use crate::runner::process::run_bash_cmd;
use user_lib::{close, open, println, write, OpenFlags};

#[cfg(target_arch = "riscv64")]
const LIBGCC_S_SO: &[u8] = include_bytes!("../../../../tools/riscv64/lib/libgcc_s.so.1");
#[cfg(target_arch = "loongarch64")]
const LIBGCC_S_SO: &[u8] = include_bytes!("../../../../tools/loongarch64/lib/libgcc_s.so.1");

pub fn link_libraries(environ: &[*const u8]) {
    println!("[initproc] linking musl/glibc libs to /lib ...");
    let lib_cmd = "\
        mkdir -p /lib /usr /lib64 /usr/lib /usr/lib64; \
        rm -rf /lib64; ln -sf /lib /lib64; \
        if [ ! -d /usr/lib ] || [ -L /usr/lib ]; then rm -rf /usr/lib 2>/dev/null || true; ln -sf /lib /usr/lib; fi; \
        rm -rf /usr/lib64; ln -sf /lib /usr/lib64; \
        mkdir -p /lib/modules/5.10.0-1-rv64 /lib/modules/5.10.0-1-la64; \
        [ -f /lib/modules/5.10.0-1-rv64/modules.dep ] || : > /lib/modules/5.10.0-1-rv64/modules.dep; \
        [ -f /lib/modules/5.10.0-1-la64/modules.dep ] || : > /lib/modules/5.10.0-1-la64/modules.dep; \
        [ -f /lib/modules/5.10.0-1-rv64/modules.builtin ] || printf '/veth.ko\n' > /lib/modules/5.10.0-1-rv64/modules.builtin; \
        [ -f /lib/modules/5.10.0-1-la64/modules.builtin ] || printf '/veth.ko\n' > /lib/modules/5.10.0-1-la64/modules.builtin; \
        [ -e /sbin/modprobe ] || ln -sf /bin/true /sbin/modprobe; \
        [ -e /bin/modprobe ] || ln -sf /bin/true /bin/modprobe; \
        [ -e /lib/ld-musl-riscv64-sf.so.1 ] || ln -sf /musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1; \
        [ -e /lib/ld-musl-riscv64.so.1 ] || ln -sf /musl/lib/libc.so /lib/ld-musl-riscv64.so.1; \
        [ -e /lib/libc.so ] || ln -sf /musl/lib/libc.so /lib/libc.so; \
        [ -e /lib/ld-linux-riscv64-lp64d.so.1 ] || ln -sf /glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1; \
        [ -e /lib/ld-linux-loongarch-lp64d.so.1 ] || ln -sf /glibc/lib/ld-linux-loongarch-lp64d.so.1 /lib/ld-linux-loongarch-lp64d.so.1; \
        [ -e /lib/ld-musl-loongarch-lp64d.so.1 ] || ln -sf /musl/lib/libc.so /lib/ld-musl-loongarch-lp64d.so.1; \
        [ -e /lib/libc.so.6 ] || ln -sf /glibc/lib/libc.so.6 /lib/libc.so.6; \
        [ -e /lib/libm.so.6 ] || ln -sf /glibc/lib/libm.so.6 /lib/libm.so.6; \
        [ -e /glibc/lib/libgcc_s.so.1 ] || ln -sf /lib/libgcc_s.so.1 /glibc/lib/libgcc_s.so.1; \
        [ -e /lib/tls_get_new-dtv_dso.so ] || ln -sf /glibc/lib/tls_get_new-dtv_dso.so /lib/tls_get_new-dtv_dso.so; \
        [ -e ./libtls_get_new-dtv_dso.so ] || ln -sf /glibc/lib/tls_get_new-dtv_dso.so ./libtls_get_new-dtv_dso.so; \
        for f in /musl/lib/*.so*; do bn=\"\x24(basename \"\x24f\")\"; case \"\x24bn\" in libgcc_s.so.1) continue;; esac; [ -e \"/lib/\x24bn\" ] || ln -sf \"\x24f\" /lib/ 2>/dev/null; done; \
        for f in /glibc/lib/*.so*; do bn=\"\x24(basename \"\x24f\")\"; case \"\x24bn\" in libgcc_s.so.1) continue;; esac; [ -e \"/lib/\x24bn\" ] || ln -sf \"\x24f\" /lib/ 2>/dev/null; done; \
    \0";
    let ret = run_bash_cmd(lib_cmd, environ);
    println!("[initproc] lib linking done, exit={}", ret);

    println!("[initproc] prepare lmbench compatibility ...");
    let lmbench_cmd = "\
        mkdir -p /code/lmbench_src/bin/build; \
        ln -s /musl/lmbench_all /code/lmbench_src/bin/build/lmbench_all \
    \0";
    let ret = run_bash_cmd(lmbench_cmd, environ);
    println!("[initproc] lmbench compatibility done, exit={}", ret);
}

/// Install embedded libgcc_s.so.1 into the glibc runtime area.
/// Idempotent: skips if the target file already exists.
pub fn install_embedded_libgcc_s() {
    let path = "/glibc/lib/libgcc_s.so.1\0";
    // Check if already installed
    let probe = open(path, OpenFlags::RDONLY);
    if probe >= 0 {
        let _ = close(probe as usize);
        println!("[initproc] install libgcc_s: already exists, skipping");
        return;
    }
    let fd = open(path, OpenFlags::WRONLY | OpenFlags::CREATE | OpenFlags::TRUNC);
    if fd < 0 {
        println!("[initproc] install libgcc_s failed to open, ret={}", fd);
        return;
    }
    let ret = write(fd as usize, LIBGCC_S_SO);
    let _ = close(fd as usize);
    if ret < 0 {
        println!("[initproc] install libgcc_s write failed, ret={}", ret);
    } else if ret as usize != LIBGCC_S_SO.len() {
        println!("[initproc] install libgcc_s short write bytes={} expected={}", ret, LIBGCC_S_SO.len());
    }
}
