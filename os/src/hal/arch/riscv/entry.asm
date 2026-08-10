    # MangoCore reserves a fixed maximum so every possible QEMU hart has a
    # private stack before Rust, the allocator, or any shared state is usable.
    .equ MAX_CPUS, 16
    .equ BOOT_STACK_SHIFT, 18
    .equ BOOT_STACK_SIZE, 1 << BOOT_STACK_SHIFT
    # The early FDT parser runs before the final kernel page table exists.
    # Keep enough temporary 1 GiB leaves for an 8 GiB QEMU guest (whose DTB is
    # placed near the top of RAM), with headroom for larger validation guests.
    .equ BOOTSTRAP_GIB_LEAVES, 16

    .section .text.entry
    .globl _start
_start:
    # OpenSBI enters S-mode with the hardware hart ID in a0. Reject an ID that
    # cannot own a slot before the first instruction that depends on a stack.
    li t0, MAX_CPUS
    bgeu a0, t0, .Lboot_park

    # Save firmware-provided registers BEFORE any other code.
    # OpenSBI passes: a0 = hartid, a1 = dtb_paddr
    la t0, RAW_HART_ID
    sd a0, 0(t0)
    la t0, RAW_DTB_PADDR
    sd a1, 0(t0)
    la t0, _start
    la t1, RAW_ENTRY_PADDR
    sd t0, 0(t1)
    la t0, riscv_image_header
    la t1, RAW_IMAGE_PADDR
    sd t0, 0(t1)
    la t0, bootstrap_root_page_table
    # Build Sv39 leaves for the physical RAM identity range and its fixed
    # high-half alias. `la` remains PC-relative, therefore it resolves to
    # physical addresses while translation is disabled.
    #
    # The RAM base is NOT hardcoded: U-Boot may load the Image at any
    # platform DRAM base (QEMU virt: 0x8000_0000; VisionFive 2: 0x4000_0000).
    # Derive the 1 GiB-aligned base from the runtime physical Image address.
    # BOOTSTRAP_GIB_LEAVES temporary 1 GiB leaves cover the high-address DTB
    # that pre-heap FDT parsing reads before mm::init().  In particular, an
    # 8 GiB QEMU guest places it near 0x27fe_0000_0; four leaves are not enough.
    # Keep the derived base in t3 across the satp switch: `la` is no longer
    # physical once translation is enabled, so the high-half alias offset is
    # computed from t3 *before* enabling the MMU.
    la t3, riscv_image_header      # runtime physical Image address (MMU off)
    li t4, 0xffffffffc0000000      # 1 GiB alignment mask
    and t3, t3, t4                 # 1 GiB-aligned DRAM base (physical)
    srli t5, t3, 12                # PPN of the DRAM base
    slli t5, t5, 10                # PPN field position in the PTE
    li t4, 0xef                    # V|R|W|X|G|A|D leaf flags
    or t1, t5, t4                  # leaf PTE: DRAM base, RWX
    # low-half identity aliases: root index = (dram_base >> 30) + i, i < 4
    srli t5, t3, 30
    slli t5, t5, 3                 # * 8 bytes per PTE
    add t5, t5, t0
    li t6, BOOTSTRAP_GIB_LEAVES
    li t4, 0x10000000
1:  sd t1, 0(t5)
    add t1, t1, t4
    addi t5, t5, 8
    addi t6, t6, -1
    bnez t6, 1b
    # high-half aliases: fixed root indices 256..259 (KERNEL_VIRT_BASE >> 30)
    li t2, 2048
    add t2, t2, t0
    srli t1, t3, 12
    slli t1, t1, 10
    li t5, 0xef
    or t1, t1, t5
    li t4, 0x10000000
    li t6, BOOTSTRAP_GIB_LEAVES
2:  sd t1, 0(t2)
    add t1, t1, t4
    addi t2, t2, 8
    addi t6, t6, -1
    bnez t6, 2b
    srli t0, t0, 12
    li t1, 0x8000000000000000
    or t0, t0, t1
    csrw satp, t0
    sfence.vma

    # Rust is linked in the high half. Convert PC-relative physical symbol
    # addresses after installing both aliases, then enter only virtually.
    # The alias offset is (KERNEL_VIRT_BASE - dram_base), computed from the
    # physical base preserved in t3 before the satp switch above.
    li t1, 0xffffffc000000000      # KERNEL_VIRT_BASE
    sub t1, t1, t3                 # virtual alias offset for this DRAM base
    # Stacks grow down, so hart N starts at the exclusive upper bound of slot N.
    # The range check above makes the add/shift bounded and keeps slots disjoint.
    # a0/a1 remain untouched for the later BSP/AP Rust entry split. On hart 0,
    # this computes the historical boot_stack_top address exactly.
    la sp, boot_stack
    addi t0, a0, 1
    slli t0, t0, BOOT_STACK_SHIFT
    add sp, sp, t0
    add sp, sp, t1
    la t0, rust_main
    add t0, t0, t1
    jr t0

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

    .section .bss.bootstrap_page_table
    .balign 4096
bootstrap_root_page_table:
    .space 4096
