//! /proc/cpuinfo — CPU 信息

use crate::fs::procfs::proc_read_str;
use crate::utils::error::SyscallErr;
use alloc::string::String;
use core::fmt::Write;

pub fn cpuinfo_content(
    _extra: usize,
    offset: usize,
    len: usize,
    buf: &mut [u8],
) -> Result<usize, SyscallErr> {
    let (arch, isa, mmu) = if cfg!(target_arch = "riscv64") {
        ("riscv64", "imafdc", "sv39")
    } else if cfg!(target_arch = "loongarch64") {
        ("loongarch64", "loongarch", "tlb")
    } else {
        ("unknown", "unknown", "unknown")
    };
    let cpu_count = crate::smp::configured_cpu_count();
    let model = crate::hal::platform::platform_info()
        .model
        .as_deref()
        .unwrap_or(if cfg!(feature = "boot_la_uboot_dmw") {
            "loongson,2k1000"
        } else {
            "unspecified"
        });
    let mut s = String::with_capacity(cpu_count.saturating_mul(128));

    // 启动门禁要求全部 configured CPU 上线后才进入用户态，所以这里按固定
    // 逻辑编号逐项输出；它与 getcpu/affinity 使用的是同一 CPU 命名空间。
    for cpu in 0..cpu_count {
        let _ = writeln!(s, "processor       : {}", cpu);
        let _ = writeln!(s, "arch            : {}", arch);
        let _ = writeln!(s, "isa             : {}", isa);
        let _ = writeln!(s, "mmu             : {}", mmu);
        let _ = writeln!(s, "uarch           : {}", model);
        if cpu + 1 != cpu_count {
            s.push('\n');
        }
    }

    proc_read_str(offset, len, buf, &s)
}
