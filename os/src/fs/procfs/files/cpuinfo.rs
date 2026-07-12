//! /proc/cpuinfo — CPU 信息

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::format;

pub fn cpuinfo_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let mut s = alloc::string::String::new();

    let (arch, isa, mmu) = if cfg!(target_arch = "riscv64") {
        ("riscv64", "imafdc", "sv39")
    } else if cfg!(target_arch = "loongarch64") {
        ("loongarch64", "loongarch", "tlb")
    } else {
        ("unknown", "unknown", "unknown")
    };

    s.push_str(&format!("processor       : 0\n"));
    s.push_str(&format!("arch            : {}\n", arch));
    s.push_str(&format!("isa             : {}\n", isa));
    s.push_str(&format!("mmu             : {}\n", mmu));
    s.push_str("uarch           : qemu-virtual\n");

    proc_read_str(offset, len, buf, &s)
}
