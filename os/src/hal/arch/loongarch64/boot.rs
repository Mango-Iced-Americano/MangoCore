//! LoongArch64 裸机入口。
//!
//! `_start` 按 CPU ID 选择启动栈并跳转到 Rust 侧 `rust_main`。

use core::arch::asm;

use crate::config::BOOT_STACK_SIZE;

// Phase 1 fixes the QEMU topology ceiling at eight contiguous CPU IDs.
const MAX_CPUS: usize = 8;

#[naked]
#[no_mangle]
#[link_section = ".text.entry"]
unsafe extern "C" fn _start() -> ! {
    // Safety: this is the first instruction stream after firmware jumps to the
    // kernel. No Rust stack or references exist yet; the assembly programs DMW,
    // validates the CPU-local ID, assigns its bounded stack, and enters Rust.
    unsafe {
        asm!(
            r"
            # 0x180 + n 为 DMWn 寄存器位置
            ori         $t0, $zero, 0x11    # 配置 CSR_DMWn_PLV0
            # lu52i.d     $t0, $t0, -2048     # UC Uncached, PLV0
            csrwr       $t0, 0x180          # 状态控制寄存器CSR_DMW0的地址
            ori         $t0, $zero, 0x11    # CSR_DMWn_MAT | CSR_DMWn_PLV0
            # lu52i.d     $t0, $t0, -1792     # CA Cached, PLV0
            csrwr       $t0, 0x181          # 状态控制寄存器CSR_DMW1的地址

            # CPUID is CPU-local, so it can be read before any shared stack exists.
            csrrd       $a0, 0x20
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

            # LA64 QEMU has no DTB/opaque boot argument in this phase.
            addi.d      $a1, $zero, 0
            # 将全局符号entry即程序入口地址rust_main地址放到临时寄存器t0中
            la.global   $t0, {entry}
            jirl        $zero,$t0,0x0

            # An invalid CPU has no safe stack and cannot call Rust or logging.
        2:
            b           2b
            ",
            max_cpus = const MAX_CPUS,
            boot_stack_size = const BOOT_STACK_SIZE,
            boot_stack = sym BOOT_STACK,
            entry = sym crate::rust_main,
            options(noreturn)
        )
    }
}

#[link_section = ".bss.stack"]
pub(crate) static mut BOOT_STACK: [u8; BOOT_STACK_SIZE * MAX_CPUS] =
    [0; BOOT_STACK_SIZE * MAX_CPUS];
