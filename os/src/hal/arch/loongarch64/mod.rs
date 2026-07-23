//! LoongArch64 HAL 后端。
//!
//! 汇总平台配置、CSR 寄存器包装、LAFlex 页表、trap、SBI 兼容层、时间源和上下文切换。

// 平台常量不可混用，尤其是 QEMU 与 2K1000 使用不同的 UART 地址和 MMIO 访问路径。
#[cfg(feature = "board_laqemu")]
#[path = "../../platform/loongarch64/qemu.rs"]
pub mod board;
#[cfg(feature = "board_2k1000")]
#[path = "../../platform/loongarch64/2k1000.rs"]
pub mod board;
pub mod config;
pub mod laflex;
#[macro_use]
mod mem_reg_macro;
mod acpi;
// boot.rs 定义 QEMU 的 `_start`。实板必须从 U-Boot 继承的 DMW 地址环境切换出去，
// 因而使用 entry.asm。
#[cfg(feature = "board_laqemu")]
mod boot;
mod sbi;
pub mod switch;
pub mod syscall_id;
pub mod time;
pub mod trap;
pub type KernelPageTableImpl = laflex::LAFlexPageTable;
pub type PageTableImpl = laflex::LAFlexPageTable;
pub use sbi::{
    console_flush, console_getchar, console_putchar, console_write_bytes, local_irq_restore,
    local_irq_save, shutdown,
};
pub use switch::__switch;
pub use tlb::{asid_alloc, asid_free, set_asid, tlb_global_invalidate, tlb_invalidate};

use crate::{
    config::{
        CPUCfg1, DIR_WIDTH, MMAP_BASE, PAGE_SIZE, PAGE_SIZE_BITS, PALEN, PTE_WIDTH, SUC_DMW_VSEG,
        VALEN,
    },
    hal::arch::loongarch64::{
        board::UART_BASE,
        trap::{set_kernel_trap_entry, set_machine_err_trap_ent},
    },
};

use self::{time::get_timer_freq_first_time, trap::strampoline};
pub use board::BLOCK_SZ;
pub use kern_stack::{
    kernel_stack_guard_slot, kstack_alloc, trap_cx_bottom_from_tid, ustack_bottom_from_tid,
    KernelStack,
};
pub use register::*;
mod kern_stack;
mod la_libc_import;
mod register;
pub mod tlb;
extern "C" {
    pub fn srfill();
}

const CPUCFG1_UAL: usize = 1 << 20;
const CPUCFG1_CRC32: usize = 1 << 25;

const CPUCFG2_FP: usize = 1 << 0;
const CPUCFG2_LSX: usize = 1 << 6;
const CPUCFG2_COMPLEX: usize = 1 << 8;
const CPUCFG2_CRYPTO: usize = 1 << 9;
const CPUCFG2_LVZ: usize = 1 << 10;
const CPUCFG2_LSPW: usize = 1 << 21;
const CPUCFG2_LAM: usize = 1 << 22;
const CPUCFG2_PTW: usize = 1 << 24;

fn read_cpucfg(index: usize) -> usize {
    let value: usize;
    // Safety: `cpucfg` only reads the CPU configuration word selected by
    // `index` into the output register.
    unsafe { core::arch::asm!("cpucfg {0},{1}", out(reg) value, in(reg) index) };
    value
}

/// Return the LoongArch ELF `AT_HWCAP` bits that userspace may safely use.
///
/// LASX and LBT are intentionally omitted even when CPUCFG reports the
/// hardware extension. The trap context currently preserves scalar FPU and
/// LSX state only.
pub fn user_hwcap() -> usize {
    const HWCAP_CPUCFG: usize = 1 << 0;
    const HWCAP_LAM: usize = 1 << 1;
    const HWCAP_UAL: usize = 1 << 2;
    const HWCAP_FPU: usize = 1 << 3;
    const HWCAP_LSX: usize = 1 << 4;
    const HWCAP_CRC32: usize = 1 << 6;
    const HWCAP_COMPLEX: usize = 1 << 7;
    const HWCAP_CRYPTO: usize = 1 << 8;
    const HWCAP_LVZ: usize = 1 << 9;
    const HWCAP_PTW: usize = 1 << 13;
    const HWCAP_LSPW: usize = 1 << 14;

    let cfg1 = read_cpucfg(1);
    let cfg2 = read_cpucfg(2);
    let mut hwcap = HWCAP_CPUCFG;

    if cfg1 & CPUCFG1_UAL != 0 {
        hwcap |= HWCAP_UAL;
    }
    if cfg1 & CPUCFG1_CRC32 != 0 {
        hwcap |= HWCAP_CRC32;
    }
    if cfg2 & CPUCFG2_LAM != 0 {
        hwcap |= HWCAP_LAM;
    }
    if cfg2 & CPUCFG2_FP != 0 {
        hwcap |= HWCAP_FPU;
    }
    if cfg2 & CPUCFG2_LSX != 0 {
        hwcap |= HWCAP_LSX;
    }
    if cfg2 & CPUCFG2_COMPLEX != 0 {
        hwcap |= HWCAP_COMPLEX;
    }
    if cfg2 & CPUCFG2_CRYPTO != 0 {
        hwcap |= HWCAP_CRYPTO;
    }
    if cfg2 & CPUCFG2_LVZ != 0 {
        hwcap |= HWCAP_LVZ;
    }
    if cfg2 & CPUCFG2_PTW != 0 {
        hwcap |= HWCAP_PTW;
    }
    if cfg2 & CPUCFG2_LSPW != 0 {
        hwcap |= HWCAP_LSPW;
    }

    hwcap
}

pub fn machine_init() {
    // remap_test not supported for lack of DMW read only privilege support
    trap::init();
    get_timer_freq_first_time();
    #[cfg(any(not(feature = "board_2k1000"), feature = "board_bringup_trace"))]
    {
        let cfg1 = CPUCfg1::read();
        boot_trace!(
            "[machine_init] address bits: hardware VALEN={} PALEN={}, build VALEN={} PALEN={}",
            cfg1.get_valen(),
            cfg1.get_palen(),
            VALEN,
            PALEN
        );
        for i in 0..=6 {
            let j: usize;
            // Safety: `cpucfg` only reads the CPU configuration word selected by
            // `i` into the output register.
            unsafe { core::arch::asm!("cpucfg {0},{1}",out(reg) j,in(reg) i) };
            boot_trace!("[CPUCFG {:#x}] {}", i, j);
        }
        for i in 0x10..=0x14 {
            let j: usize;
            // Safety: same read-only CPUCFG access as above.
            unsafe { core::arch::asm!("cpucfg {0},{1}",out(reg) j,in(reg) i) };
            boot_trace!("[CPUCFG {:#x}] {}", i, j);
        }
        boot_trace!("{:?}", Misc::read());
        boot_trace!("{:?}", RVACfg::read());
        boot_trace!("[machine_init] MMAP_BASE: {:#x}", MMAP_BASE);
    }
    trap::enable_timer_interrupt();
}
pub fn pre_start_init() {
    EEntry::empty().set_exception_entry(strampoline as usize);
}
#[no_mangle]
pub fn bootstrap_init() {
    if CPUId::read().get_core_id() != 0 {
        loop {}
    };
    ECfg::empty()
        .set_line_based_interrupt_vector(LineBasedInterrupt::TIMER)
        .write();
    let cfg2 = read_cpucfg(2);
    EUEn::read()
        .set_float_point_stat(cfg2 & CPUCFG2_FP != 0)
        .set_simd_extension_enabled(cfg2 & CPUCFG2_LSX != 0)
        // LASX state is not part of the trap context yet.
        .set_advanced_simd_extension_enabled(false)
        .write();
    // Timer & other Interrupts
    TIClr::read().clear_timer().write();
    TCfg::read().set_enable(false).write();
    // 地址布局按 CPUCFG1 的完整 VALEN 构建，不能继承固件选择的缩减虚拟地址模式；
    // 否则按构建期 VALEN 判断为规范的地址仍可能在页表转换前触发异常。
    RVACfg::read().set_rbits(0).write();
    CrMd::read()
        .set_watchpoint_enabled(false)
        .set_paging(true)
        .set_ie(false)
        .write();

    // Trap/Exception Hanlder initialization.
    set_kernel_trap_entry();
    set_machine_err_trap_ent();
    TLBREntry::read().set_addr(srfill as usize).write();

    // MMU Setup
    DMW2::read()
        .set_plv0(true)
        .set_plv1(false)
        .set_plv2(false)
        .set_plv3(false)
        .set_vesg(SUC_DMW_VSEG)
        .set_mat(MemoryAccessType::StronglyOrderedUnCached)
        .write();
    DMW3::empty().write();
    //DMW1::empty().write();

    // STLBPS 保存页面大小以 2 为底的对数，4KiB 对应 12。TLBREHi 包装接口接收字节数并在
    // 内部生成同一编码。两者都与 PTE_WIDTH_BITS=3 无关，后者只表示 PTE 为 8 字节。
    STLBPS::read().set_ps(PAGE_SIZE_BITS).write();
    TLBREHi::read().set_page_size(PAGE_SIZE).write();
    PWCL::read()
        .set_ptbase(PAGE_SIZE_BITS)
        .set_ptwidth(DIR_WIDTH)
        .set_dir1_base(PAGE_SIZE_BITS + DIR_WIDTH)
        .set_dir1_width(DIR_WIDTH) // 512*512*4096 should be enough for 256MiB of 2k500.
        .set_dir2_base(0)
        .set_dir2_width(0)
        .set_pte_width(PTE_WIDTH)
        .write();
    PWCH::read()
        .set_dir3_base(PAGE_SIZE_BITS + DIR_WIDTH * 2)
        .set_dir3_width(DIR_WIDTH)
        .set_dir4_base(0)
        .set_dir4_width(0)
        .write();

    boot_trace!("[kernel] UART address: {:#x}", UART_BASE);
    let cfg1 = CPUCfg1::read();
    boot_trace!(
        "[bootstrap_init] address bits: hardware VALEN={} PALEN={}, build VALEN={} PALEN={}",
        cfg1.get_valen(),
        cfg1.get_palen(),
        VALEN,
        PALEN
    );
    // 如果镜像按错误的开发板配置构建，应在启用正常内存分配前停止。继续运行会让
    // PTE 掩码和规范地址检查与 CPU 不一致，使后续异常表现为误导性的 MMU 故障。
    assert_eq!(
        cfg1.get_valen(),
        VALEN,
        "kernel VALEN does not match CPUCFG1"
    );
    assert_eq!(
        cfg1.get_palen(),
        PALEN,
        "kernel PALEN does not match CPUCFG1"
    );
    boot_trace!("[bootstrap_init] {:?}", PRCfg1::read());
}
