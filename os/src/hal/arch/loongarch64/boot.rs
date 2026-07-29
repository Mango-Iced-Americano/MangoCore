//! LoongArch64 裸机入口。
//!
//! `_start` 设置启动栈并跳转到 Rust 侧 `rust_main`。

use core::arch::naked_asm;

use crate::config::BOOT_STACK_SIZE;

// SAFETY: [Category 13 — library/unsafe contract] Firmware enters this as the
// first LoongArch instruction stream, before a Rust stack or references exist.
// The assembly initializes DMW and `$sp`, then tail-jumps to `rust_main`.
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.entry"]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
            // 以下内容看不懂的话
            // 可以去看龙架构手册卷一 v1.11
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
            # 保存栈底地址
            la.global   $sp, {boot_stack}

            # 将立即数boot_stack_size即引导栈的大小加载到临时寄存器t0中
            li.d        $t0, {boot_stack_size}

            add.d       $sp, $sp, $t0
            csrrd       $a0, 0x20
            # 将全局符号entry即程序入口地址rust_main地址放到临时寄存器t0中
            la.global   $t0, {entry}
            jirl        $zero,$t0,0x0
            ",
            raw_hart_id = sym crate::hal::boot::RAW_HART_ID,
            raw_dtb_paddr = sym crate::hal::boot::RAW_DTB_PADDR,
            boot_stack_size = const BOOT_STACK_SIZE,
            boot_stack = sym BOOT_STACK,
            entry = sym crate::rust_main,
    )
}

#[link_section = ".bss.stack"]
pub(crate) static mut BOOT_STACK: [u8; BOOT_STACK_SIZE] = [0; BOOT_STACK_SIZE];
