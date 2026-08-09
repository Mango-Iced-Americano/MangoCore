//! RISC-V HAL 后端。
//!
//! 包含统一 FDT 启动配置（与 SMP 并发基础设施）、SV39 页表、trap、SBI、
//! 时钟和上下文切换实现。

use core::arch::naked_asm;

pub mod config;
pub mod plic;
pub mod kern_stack;
pub mod reset;
pub mod sbi;
pub mod sv39;
pub mod switch;
pub mod syscall_id;
pub mod time;
pub mod trap;

pub use kern_stack::reclaim_retired_kernel_stacks;

pub fn machine_init() {
    trap::init();
    trap::enable_ipi_interrupt();
    let user_asids = sv39::init_asid_allocator();
    if user_asids == 0 {
        crate::println!("[mm] RISC-V SATP.ASID unavailable; using switch-time TLB flushes");
    } else {
        crate::println!("[mm] RISC-V ASID allocator: {} user IDs", user_asids);
    }
    if crate::smp::configured_cpu_count() > 1 {
        match sbi::init_rfence() {
            Ok(true) => crate::println!("[smp] SBI RFENCE enabled for user page shootdown"),
            Ok(false) => crate::println!("[smp] SBI RFENCE unavailable; using IPI fallback"),
            Err(error) => crate::println!(
                "[smp] SBI RFENCE probe failed ({}); using IPI fallback",
                error
            ),
        }
    }
    // 当前 CPU 的第一个 deadline 会在开放 STIE 前由 timer_cpu_init() 写入。
}

use time::set_next_trigger;

pub use trap::context::MachineContext;

/// 在 deadline 已经写入后开放当前 hart 的 supervisor timer interrupt。
pub fn enable_local_timer_interrupt() {
    trap::enable_timer_interrupt();
}

pub type KernelPageTableImpl = sv39::Sv39PageTable;
pub type PageTableImpl = sv39::Sv39PageTable;
pub type TrapImpl = riscv::register::scause::Trap;
pub type InterruptImpl = riscv::register::scause::Interrupt;
pub type ExceptionImpl = riscv::register::scause::Exception;

/// 清除当前 hart 能观察到的全部内核地址翻译。
///
/// 不带地址和 ASID 的 `sfence.vma` 同时覆盖 global 与 non-global 项，适合
/// kernel-global 动态映射撤销后的保守同步。
pub fn kernel_tlb_invalidate() {
    sv39::tlb_invalidate();
}

/// RV64 恒等映射具有真实叶子 PTE，dirty 位直接保存在 PTE 中。
pub fn init_kernel_mapping_metadata() {}

/// 清除当前 hart 上可能属于任意用户地址空间的翻译。
///
/// 全 MM 同步和 ASID rollover 仍需要无操作数 `sfence.vma`；它也会清除 global
/// 项，因此只用于明确要求全刷的慢路径。
pub fn user_tlb_invalidate() {
    sv39::tlb_invalidate();
}

/// 清除当前 hart 上指定 MM、指定用户虚拟页的 non-global 翻译。
pub fn user_tlb_invalidate_page(asid: u16, vpn: crate::mm::VirtPageNum) {
    sv39::tlb_invalidate_addr_asid(usize::from(crate::mm::VirtAddr::from(vpn)), asid);
}

/// 清除当前 hart 上指定 MM 的有界用户页区间。
pub fn user_tlb_invalidate_range(asid: u16, range: crate::mm::VPNRange) {
    debug_assert!(range.get_end().0 - range.get_start().0 <= crate::smp::MAX_USER_TLB_RANGE_PAGES);
    for vpn in range {
        user_tlb_invalidate_page(asid, vpn);
    }
}

/// 通过 SBI RFENCE 同步失效一组逻辑 CPU 上的指定用户页区间。
///
/// `Ok(false)` 只表示固件缺少 RFENCE；上层仍须执行软件 IPI fallback。
pub fn remote_user_tlb_invalidate_range(
    targets: usize,
    asid: u16,
    range: crate::mm::VPNRange,
) -> Result<bool, isize> {
    let hart_mask = crate::smp::logical_to_hardware_mask(targets);
    let pages = range.get_end().0 - range.get_start().0;
    debug_assert!((1..=crate::smp::MAX_USER_TLB_RANGE_PAGES).contains(&pages));
    let start = usize::from(crate::mm::VirtAddr::from(range.get_start()));
    let size = pages
        .checked_mul(crate::config::PAGE_SIZE)
        .expect("bounded user TLB range size overflowed");
    sbi::remote_sfence_vma_asid(hart_mask, start, size, asid)
}

pub fn bootstrap_init(cpu_id: usize) {
    if cpu_id != crate::smp::BOOT_CPU_ID {
        // AP 先只开放 software interrupt；本地调度器建立首个 deadline 后
        // 才单独加入 timer source，external interrupt 仍保持关闭。
        trap::init_ipi_only();
    }
}

#[repr(C, align(4096))]
struct IdleStacks([u8; config::KERNEL_STACK_SIZE * crate::smp::MAX_CPUS]);

// idle stack 直到 CPU0 清完 BSS 才会启用，因此它应位于 sbss 之后，而不是和
// 固件入口正在使用的 boot stack 一样逃过清零。链接脚本的 `.bss.*` 通配符
// 会把该数组保留在内核镜像范围内，物理页分配器也不会回收这段内存。
// 数组按编译期上限 `MAX_CPUS` 定界（Linux __per_cpu_offset[NR_CPUS] 模式）；
// 实际使用的槽数由运行时的 `runtime_cpu_count()` 决定。
#[link_section = ".bss.idle_stack"]
static mut IDLE_STACKS: IdleStacks = IdleStacks([0; config::KERNEL_STACK_SIZE * crate::smp::MAX_CPUS]);

/// 抛弃当前 boot stack，并在指定 CPU 的 idle stack 上进入 Rust。
pub fn enter_secondary_idle(cpu_id: usize, entry: extern "C" fn(usize) -> !) -> ! {
    assert!(cpu_id < crate::smp::MAX_CPUS);

    // 只取得静态区的裸地址，不为 `static mut` 创建共享引用；每个 CPU 根据
    // logical ID 独占一个固定槽，切栈后也不会与其他 CPU 产生别名写入。
    let base = core::ptr::addr_of!(IDLE_STACKS).cast::<u8>() as usize;
    let stack_top = base + (cpu_id + 1) * config::KERNEL_STACK_SIZE;
    debug_assert_eq!(stack_top & 0xf, 0);

    // Safety: cpu_id 已完成边界检查，stack_top 指向对应槽的上界；跳转目标
    // 永不返回，因此旧栈上的 Rust frame 不会再被访问。
    unsafe { switch_secondary_stack(cpu_id, stack_top, entry) }
}

#[unsafe(naked)]
unsafe extern "C" fn switch_secondary_stack(
    _cpu_id: usize,
    _stack_top: usize,
    _entry: extern "C" fn(usize) -> !,
) -> ! {
    naked_asm!(
        "mv sp, a1",
        // 新 idle 调用链不应沿用 boot stack 的 frame/return 链。
        "mv s0, zero",
        "mv ra, zero",
        // a0 保留 logical CPU ID，tp 保留 PerCpu 指针。
        "jr a2",
    )
}

/// Install the current CPU's kernel-local anchor in `tp`.
pub fn install_cpu_local(ptr: usize) {
    // Safety: the psABI makes x4/tp non-allocatable to compiler temporaries,
    // while the user trap path saves user TLS before reinstalling this value.
    unsafe {
        core::arch::asm!("mv tp, {ptr}", ptr = in(reg) ptr, options(nostack));
    }
}

/// Read the kernel-local anchor after boot or user-trap entry installed it.
pub fn cpu_local_ptr() -> usize {
    let ptr;
    // Safety: this is a read-only move from the same CPU-local register.
    unsafe {
        core::arch::asm!("mv {ptr}, tp", ptr = out(reg) ptr, options(nostack));
    }
    ptr
}

/// Ask OpenSBI HSM to enter the common assembly entry on one stopped hart.
pub fn start_secondary_cpu(cpu_id: usize, start_addr: usize) -> Result<(), isize> {
    // HSM passes its opaque argument in a1.  Phase 1 does not consume an
    // architecture boot argument on APs, so publish an explicit zero.
    sbi::hart_start(cpu_id, start_addr, 0)
}

/// 向一个硬件 hart 发送运行期 IPI doorbell。
pub fn send_ipi(hardware_id: usize) -> Result<(), isize> {
    sbi::send_ipi(hardware_id)
}

/// 在全局 SIE 关闭时等待一个局部已使能的中断。
///
/// RISC-V 规定 WFI 必须因局部 enabled+pending 的中断恢复，不受全局 SIE
/// 影响；CPU0 与 AP 调度器都在返回后恢复 SIE，让 pending source 真正进入 trap。
pub fn cpu_wait_for_interrupt() {
    // Safety: WFI 只暂停当前 hart，不访问内存或改变中断 mask。
    unsafe { riscv::asm::wfi() };
}

/// 为终态 stop 清除全部 supervisor 本地中断使能。
pub fn prepare_secondary_cpu_stop() {
    // Safety: 再次清除全局 SIE，使这个 HAL 边界不依赖调用方；随后清空
    // `sie`，没有本地 source 能使 WFI 因 enabled+pending 条件恢复。
    unsafe {
        core::arch::asm!("csrci sstatus, 2", "csrw sie, zero", options(nostack));
    }
}

/// 在全局中断已关闭后永久停止当前 AP。
pub fn secondary_cpu_stop() -> ! {
    loop {
        // 即使实现允许 WFI 无理由返回，也只会再次进入 WFI；本函数永不
        // 恢复中断、返回 Rust 调用者或访问共享状态。
        unsafe { riscv::asm::wfi() };
    }
}

/// Park an unexpected/unconfigured CPU outside all shared runtime code.
pub fn boot_cpu_park() -> ! {
    loop {
        // Safety: WFI is only a local processor hint.  AP interrupts remain
        // disabled, and a later IPI phase will replace this permanent park.
        unsafe { riscv::asm::wfi() };
    }
}

/// Return the Linux-compatible RISC-V ISA-letter bitmap for `AT_HWCAP`.
pub fn user_hwcap() -> usize {
    // IMAFDC, with the bit position derived from the extension letter.
    0x112d
}
