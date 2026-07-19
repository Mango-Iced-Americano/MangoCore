//! /proc/version — 内核版本信息

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::format;

pub fn version_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let arch = if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else if cfg!(target_arch = "loongarch64") {
        "loongarch64"
    } else {
        "unknown"
    };
    let s = alloc::format!("OSKernel2026-Mango 0.1.0 ({})\n", arch);
    proc_read_str(offset, len, buf, &s)
}
