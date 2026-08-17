//! LoongArch64 裸机入口。
//!
//! `_start` 按 CPU ID 选择启动栈并跳转到 Rust 侧 `rust_main`。

use core::arch::naked_asm;

use crate::config::BOOT_STACK_SIZE;

// Must match the architecture-neutral per-CPU table ceiling.  The early entry
// cannot refer to Rust state before assigning a private boot stack.
const MAX_CPUS: usize = crate::smp::MAX_CPUS;

// SAFETY: 固件在尚无 Rust 栈和引用时进入这里。汇编只配置 DMW、校验本地
// CPUID、选择边界内的启动栈，最后跳转到 `rust_main`。
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.entry"]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
            r"
            # Save firmware handoff registers BEFORE DMW setup and csrrd.
            la.global   $t0, {raw_hart_id}
            st.d        $a0, $t0, 0
            la.global   $t0, {raw_dtb_paddr}
            st.d        $a1, $t0, 0
            # Original DMW and boot sequence.
            # 0x180 + n 为 DMWn 寄存器位置
            ori         $t0, $zero, 0x11    # 配置 CSR_DMWn_PLV0
            # lu52i.d     $t0, $t0, -2048     # UC Uncached, PLV0
            csrwr       $t0, 0x180          # 状态控制寄存器CSR_DMW0的地址
            ori         $t0, $zero, 0x11    # CSR_DMWn_MAT | CSR_DMWn_PLV0
            # lu52i.d     $t0, $t0, -1792     # CA Cached, PLV0
            csrwr       $t0, 0x181          # 状态控制寄存器CSR_DMW1的地址

            # QEMU direct boot 在 a2 传入 EFI system table。先把它整理成
            # rust_main 的第二个参数；后续读取 CPUID 会覆盖 a0，但不会影响 a1。
            addi.d      $a1, $a2, 0
            # CPUID is CPU-local, so it can be read before any shared stack exists.
            csrrd       $a0, 0x20
            # 覆盖开头用固件 a0（QEMU 恒为 1 的 EFI 标志，非 hart id）写入的
            # RAW_HART_ID：Linux LoongArch 启动 ABI 中 a0 是 flags 而非 CPU 编号，
            # 真正的 boot 核编号在 CSR 0x20。不覆盖会让 count_cpus 误把 FDT 之外
            # 的 hart 计数为第 2 个逻辑核（la64 -smp 1 时 expected=0x3 挂起）。
            la.global   $t0, {raw_hart_id}
            st.d        $a0, $t0, 0
            # Reject an ID without a reserved slot before assigning or using $sp.
            li.d        $t0, {max_cpus}
            bgeu        $a0, $t0, 2f

            # Every legal CPU starts from the same array base.
            la.global   $sp, {boot_stack}
            # Stack N owns [base + N * size, base + (N + 1) * size).
            addi.d      $t1, $a0, 1
            # Multiplication keeps the stride tied to BOOT_STACK_SIZE.
            li.d        $t0, {boot_stack_size}
            mul.d       $t0, $t0, $t1
            # Stacks grow down, so $sp begins at the slot's exclusive upper bound.
            add.d       $sp, $sp, $t0

            # 将全局符号entry即程序入口地址rust_main地址放到临时寄存器t0中
            la.global   $t0, {entry}
            jirl        $zero,$t0,0x0

            # An invalid CPU has no safe stack and cannot call Rust or logging.
        2:
            b           2b
            ",
            max_cpus = const MAX_CPUS,
            raw_hart_id = sym crate::hal::boot::RAW_HART_ID,
            raw_dtb_paddr = sym crate::hal::boot::RAW_DTB_PADDR,
            boot_stack_size = const BOOT_STACK_SIZE,
            boot_stack = sym BOOT_STACK,
            entry = sym crate::rust_main,
    )
}

#[link_section = ".bss.stack"]
pub(crate) static mut BOOT_STACK: [u8; BOOT_STACK_SIZE * MAX_CPUS] =
    [0; BOOT_STACK_SIZE * MAX_CPUS];
