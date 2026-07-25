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

#[cfg(feature = "board_laqemu")]
const IOCSR_IPI_ENABLE: usize = 0x1004;
#[cfg(feature = "board_laqemu")]
const IOCSR_IPI_CLEAR: usize = 0x100c;
#[cfg(feature = "board_laqemu")]
const IOCSR_IPI_SEND: usize = 0x1040;
#[cfg(feature = "board_laqemu")]
const RUNTIME_IPI_VECTOR: u32 = 1;

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
pub fn bootstrap_init(cpu_id: usize) {
    let local_interrupt = if cpu_id == crate::smp::BOOT_CPU_ID {
        LineBasedInterrupt::TIMER
    } else {
        // AP 在当前子阶段只允许 IPI，不能提前接入普通 timer callback。
        LineBasedInterrupt::IPI
    };
    ECfg::empty()
        .set_line_based_interrupt_vector(local_interrupt)
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

    // The raw UART singleton is not yet cross-CPU serialized.  CPU0 validates
    // the homogeneous QEMU/board configuration; APs stay silent and only
    // publish their online bit after this local routine returns.
    if cpu_id == 0 {
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
    } else {
        configure_local_ipi();
        // IPI device、ECFG 和内核 trap entry 均已就绪后才打开全局 IE；
        // CPU 仍未 online，因此 BSP 不可能在该窗口提前发送运行期 IPI。
        CrMd::read().set_ie(true).write();
    }
}

#[repr(C, align(4096))]
struct IdleStacks([u8; config::KERNEL_STACK_SIZE * crate::smp::configured_cpu_count()]);

// AP 只有在 CPU0 清完 BSS 并发布 BOOT_PHASE 后才会进入 idle stack，所以该
// 数组放在普通 `.bss.*` 内。它仍属于内核镜像，不会被物理页分配器重新分配。
#[link_section = ".bss.idle_stack"]
static mut IDLE_STACKS: IdleStacks =
    IdleStacks([0; config::KERNEL_STACK_SIZE * crate::smp::configured_cpu_count()]);

/// 抛弃当前 boot stack，并在指定 CPU 的 idle stack 上进入 Rust。
pub fn enter_secondary_idle(cpu_id: usize, entry: extern "C" fn(usize) -> !) -> ! {
    assert!(cpu_id < crate::smp::configured_cpu_count());

    // 使用裸地址避免创建指向 `static mut` 的引用。KERNEL_STACK_SIZE 为页倍数，
    // 数组本身按页对齐，因此每个槽的顶部同时满足 LA64 ABI 的 16 字节对齐。
    let base = core::ptr::addr_of!(IDLE_STACKS).cast::<u8>() as usize;
    let stack_top = base + (cpu_id + 1) * config::KERNEL_STACK_SIZE;
    debug_assert_eq!(stack_top & 0xf, 0);

    // Safety: 目标槽由 cpu_id 独占，entry 永不返回，切栈后不会再读取旧 frame。
    unsafe { switch_secondary_stack(cpu_id, stack_top, entry) }
}

#[unsafe(naked)]
unsafe extern "C" fn switch_secondary_stack(
    _cpu_id: usize,
    _stack_top: usize,
    _entry: extern "C" fn(usize) -> !,
) -> ! {
    core::arch::naked_asm!(
        "addi.d $sp, $a1, 0",
        // 清除旧 boot stack 的 frame/return 链，便于 panic 回溯在新根处终止。
        "addi.d $fp, $zero, 0",
        "addi.d $ra, $zero, 0",
        // a0 保留 logical CPU ID，r21 保留 PerCpu 指针。
        "jirl $zero, $a2, 0",
    )
}

/// Install the current CPU's kernel-local anchor in reserved register r21.
pub fn install_cpu_local(ptr: usize) {
    // Safety: r21 is non-allocatable in the psABI, but privilege entry does
    // not switch GPRs, so user trap entry explicitly reinstalls this value.
    unsafe {
        core::arch::asm!("move $r21, {ptr}", ptr = in(reg) ptr, options(nostack));
    }
}

/// Read the kernel-local anchor after boot or user-trap entry installed it.
pub fn cpu_local_ptr() -> usize {
    let ptr;
    // Safety: this is a read-only move from the same CPU-local register.
    unsafe {
        core::arch::asm!("move {ptr}, $r21", ptr = out(reg) ptr, options(nostack));
    }
    ptr
}

/// Wake one LA64 QEMU AP from the slave boot ROM and send it to `_start`.
#[cfg(feature = "board_laqemu")]
pub fn start_secondary_cpu(cpu_id: usize, start_addr: usize) -> Result<(), isize> {
    // QEMU 9.2.1's direct-boot ROM waits for IPI vector 0, then reads the
    // 64-bit entry address from the target CPU's first mailbox buffer.
    const IOCSR_MAIL_SEND: usize = 0x1048;

    // The QEMU ROM consumes a physical entry that fits in its low mailbox word.
    if start_addr > u32::MAX as usize {
        return Err(-3);
    }
    // MAIL_SEND encodes the target CPU in bits 16..25 and one 32-bit mailbox
    // word in bits 32..63. Offset zero selects the low half of CORE_BUF_20.
    let mail = ((start_addr as u64) << 32) | ((cpu_id as u64) << 16);
    // IPI_SEND uses the same target field; vector zero occupies bits 0..4.
    let doorbell = (cpu_id as u32) << 16;

    // Safety: these IOCSR addresses are the architected QEMU virt IPI device.
    // CPU0 is the sole startup writer, and cpu_id is bounded by MAX_CPUS.
    unsafe {
        core::arch::asm!(
            "iocsrwr.d {mail}, {mail_addr}",
            mail = in(reg) mail,
            mail_addr = in(reg) IOCSR_MAIL_SEND,
        );
        // Do not let the AP observe the doorbell before its entry mailbox.
        core::arch::asm!("dbar 0");
        core::arch::asm!(
            "iocsrwr.w {doorbell}, {ipi_addr}",
            doorbell = in(reg) doorbell,
            ipi_addr = in(reg) IOCSR_IPI_SEND,
        );
    }
    Ok(())
}

/// 向一个硬件 CPU 发送运行期 IPI；vector 1 与 slave ROM 的 vector 0 分离。
#[cfg(feature = "board_laqemu")]
pub fn send_ipi(hardware_id: usize) -> Result<(), isize> {
    if hardware_id >= crate::smp::configured_cpu_count() {
        return Err(-3);
    }
    let doorbell = ((hardware_id as u32) << 16) | RUNTIME_IPI_VECTOR;
    // Safety: IOCSR_IPI_SEND 是 QEMU virt IPI 控制器的跨核 doorbell。
    unsafe {
        core::arch::asm!("dbar 0");
        core::arch::asm!(
            "iocsrwr.w {doorbell}, {addr}",
            doorbell = in(reg) doorbell,
            addr = in(reg) IOCSR_IPI_SEND,
        );
    }
    Ok(())
}

#[cfg(feature = "board_2k1000")]
pub fn send_ipi(_hardware_id: usize) -> Result<(), isize> {
    Err(-2)
}

#[cfg(feature = "board_laqemu")]
fn configure_local_ipi() {
    let bit = 1u32 << RUNTIME_IPI_VECTOR;
    // boot ROM 已在跳转前清除启动 vector 0；这里清掉可能残留的运行期
    // vector 1，再只开放这一条 IPI 线路。
    unsafe {
        core::arch::asm!(
            "iocsrwr.w {bit}, {addr}",
            bit = in(reg) bit,
            addr = in(reg) IOCSR_IPI_CLEAR
        );
        core::arch::asm!(
            "iocsrwr.w {bit}, {addr}",
            bit = in(reg) bit,
            addr = in(reg) IOCSR_IPI_ENABLE
        );
    }
}

#[cfg(feature = "board_2k1000")]
fn configure_local_ipi() {}

/// 清除本 CPU 的 level-triggered 运行期 IPI 源。
#[cfg(feature = "board_laqemu")]
pub(super) fn clear_local_ipi() {
    let bit = 1u32 << RUNTIME_IPI_VECTOR;
    // Safety: CORE_CLEAR 写 1 清对应 status bit；handler 不访问普通锁或堆。
    unsafe {
        core::arch::asm!(
            "iocsrwr.w {bit}, {addr}",
            bit = in(reg) bit,
            addr = in(reg) IOCSR_IPI_CLEAR
        );
    }
}

#[cfg(feature = "board_2k1000")]
pub(super) fn clear_local_ipi() {}

/// AP 的 IPI-only idle loop；内核 trap 返回后继续等待下一次 doorbell。
pub fn secondary_cpu_idle() -> ! {
    loop {
        // Safety: `idle 0` 只暂停当前 CPU，全局 IE 和 IPI mask 已完成本地配置。
        unsafe { core::arch::asm!("idle 0") };
    }
}

/// The 2K1000LA remains intentionally single-core in this QEMU-only phase.
#[cfg(feature = "board_2k1000")]
pub fn start_secondary_cpu(_cpu_id: usize, _start_addr: usize) -> Result<(), isize> {
    Err(-2)
}

/// Keep an online Phase 1 AP outside interrupts and shared runtime code.
pub fn boot_cpu_park() -> ! {
    loop {
        // LoongArch idle/IPI wakeup is introduced in Phase 2.  A spin hint is
        // deliberately used here so this minimal park has no wakeup contract.
        core::hint::spin_loop();
    }
}
