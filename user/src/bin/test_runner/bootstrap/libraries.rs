use user_lib::{close, open, println, write, OpenFlags};

#[cfg(target_arch = "riscv64")]
const LIBGCC_S_SO: &[u8] = include_bytes!("../../../../tools/riscv64/lib/libgcc_s.so.1");
#[cfg(target_arch = "loongarch64")]
const LIBGCC_S_SO: &[u8] = include_bytes!("../../../../tools/loongarch64/lib/libgcc_s.so.1");

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
