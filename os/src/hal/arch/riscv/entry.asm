    .section .text.entry
    .globl _start
_start:
    # Save firmware-provided registers BEFORE any other code.
    # OpenSBI passes: a0 = hartid, a1 = dtb_paddr
    la t0, RAW_HART_ID
    sd a0, 0(t0)
    la t0, RAW_DTB_PADDR
    sd a1, 0(t0)
    # Original boot sequence.
    la sp, boot_stack_top
    # a0, a1 already set by firmware — they become the first two args to rust_main.
    call rust_main

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space 4096 * 64
    .globl boot_stack_top
boot_stack_top:
