#!/bin/sh
# Contract: RV64 has one relocatable FDT/Image boot path. This deliberately
# inspects only active RV64 source/build inputs; the artifact contract proves
# that the resulting binary observes the same ABI.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
failures=0

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

forbidden='BOARD[[:space:]]*[:?]?=|BOOT_PROFILE|boot_rv_sbi_fdt|boot_rv_uboot_go|UbootGo|KERNEL_ENTRY_PA|linker-rvqemu\.ld|linker-vf2\.ld'
if grep -E -R -n --include='*.rs' --include='*.mk' --include='*.ld' \
    --include='*.S' --include='*.asm' --include='*.toml' "$forbidden" \
    "$root/os/make/arch/rv64-settings.mk" "$root/os/make/rv64.mk" \
    "$root/os/build.rs" "$root/os/Cargo.toml" "$root/os/src/hal/arch/riscv" \
    "$root/os/src/hal/boot" "$root/os/src/hal/firmware" "$root/os/src/hal/platform" \
    "$root/os/src/main.rs" "$root/os/src/drivers" "$root/os/src/kernel_tests" \
    "$root/os/src/task/processor.rs" >/dev/null; then
    fail 'RV64 source/build path retains a board or boot-profile selector'
fi

if [ ! -f "$root/os/src/hal/arch/riscv/image_header.S" ]; then
    fail 'RV64 Linux Image header assembly is missing'
fi

if ! grep -q 'KERNEL_LINK_VADDR' "$root/os/src/hal/arch/riscv/linker.ld"; then
    fail 'RV64 linker must use a fixed high virtual address'
fi

if grep -E -q '^[[:space:]]*\.[[:space:]]*=[[:space:]]*0[[:space:]]*;' "$root/os/src/hal/arch/riscv/linker.ld"; then
    fail 'RV64 linker must not link Rust at physical VMA zero'
fi

if ! grep -q 'csrw satp' "$root/os/src/hal/arch/riscv/entry.asm"; then
    fail 'RV64 entry must enable Sv39 before entering Rust'
fi

if ! grep -q 'bootstrap_root_page_table' "$root/os/src/hal/arch/riscv/entry.asm"; then
    fail 'RV64 entry must install bootstrap identity and high mappings'
fi

if ! grep -q 'la t0, bootstrap_root_page_table' "$root/os/src/hal/arch/riscv/entry.asm"; then
    fail 'RV64 entry must write bootstrap leaves through the bootstrap page table'
fi

if ! grep -q '\*(\.bss\.bootstrap_page_table)' "$root/os/src/hal/arch/riscv/linker.ld"; then
    fail 'RV64 linker must preserve the active bootstrap page table before sbss'
fi

if ! grep -q 'RiscvFdt' "$root/os/src/hal/boot/mod.rs"; then
    fail 'RV64 boot protocol must identify standard RISC-V FDT handoff'
fi

grep -q 'RV64 boot FDT validation or pre-heap discovery failed' "$root/os/src/hal/firmware/mod.rs" ||
    fail 'RV64 must fail closed when FDT validation cannot complete'

if ! grep -q 'dtb_paddr & 0x3 == 0' "$root/os/src/hal/firmware/mod.rs"; then
    fail 'RV64 must accept a word-aligned standard FDT address from firmware'
fi

if grep -q 'dtb_paddr & 0xFFF' "$root/os/src/hal/firmware/mod.rs"; then
    fail 'RV64 must not require a page-aligned FDT address'
fi

if ! grep -q 'paddr & 0x3 != 0' "$root/os/src/hal/firmware/fdt.rs"; then
    fail 'FDT header volatile u32 reads require word alignment only'
fi

if grep -q 'frame_reclaim_linker_range' "$root/os/src/fs/initramfs.rs"; then
    fail 'embedded initramfs pages share the live kernel image and must not be reclaimed'
fi

if [ "$failures" -ne 0 ]; then
    printf 'RV64 standard FDT boot contract: RED (%s failure(s))\n' "$failures" >&2
    exit 1
fi

printf 'RV64 standard FDT boot contract: PASS\n'
