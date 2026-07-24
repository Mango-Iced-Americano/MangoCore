    # MangoCore reserves a fixed maximum so every possible QEMU hart has a
    # private stack before Rust, the allocator, or any shared state is usable.
    .equ MAX_CPUS, 8
    .equ BOOT_STACK_SHIFT, 18
    .equ BOOT_STACK_SIZE, 1 << BOOT_STACK_SHIFT

    .section .text.entry
    .globl _start
_start:
    # OpenSBI enters S-mode with the hardware hart ID in a0. Reject an ID that
    # cannot own a slot before the first instruction that depends on a stack.
    li t0, MAX_CPUS
    bgeu a0, t0, .Lboot_park

    # Stacks grow down, so hart N starts at the exclusive upper bound of slot N.
    # The range check above makes the add/shift bounded and keeps slots disjoint.
    la sp, boot_stack
    addi t0, a0, 1
    slli t0, t0, BOOT_STACK_SHIFT
    add sp, sp, t0

    # a0/a1 remain untouched for the later BSP/AP Rust entry split. On hart 0,
    # this computes the historical boot_stack_top address exactly.
    call rust_main

.Lboot_park:
    # An invalid hart has no safe stack and must not call logging or Rust.
    # rust_main is divergent; parking also contains an unexpected return.
    wfi
    j .Lboot_park

    .section .bss.stack
    .balign 4096
    .globl boot_stack
boot_stack:
    # Keep the first slot separate so boot_stack_top remains CPU0's upper bound;
    # panic backtraces therefore retain their existing single-core bounds.
    .space BOOT_STACK_SIZE
    .globl boot_stack_top
boot_stack_top:
    # The complete array stays before linker symbol sbss, so BSP BSS clearing
    # cannot erase a stack that an AP may already be using.
    .space BOOT_STACK_SIZE * (MAX_CPUS - 1)
    .globl boot_stack_upper_bound
boot_stack_upper_bound:
