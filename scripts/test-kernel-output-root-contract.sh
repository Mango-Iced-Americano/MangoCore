#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

overall=0

pass() {
    echo "PASS: $*"
}

fail() {
    echo "FAIL: $*" >&2
    overall=1
}

require_kernel_output_root_probe() {
    target=$1
    arch=$2
    profile=$3
    output_root=$4
    target_triple=$5
    alias=$6
    expected_elf="$output_root/$target_triple/release/os"
    expected_bin="$expected_elf.bin"

    if output=$(make -C "$repo_root/os" -n "ARCH=$arch" "PROFILE=$profile" "KERNEL_OUTPUT_ROOT=$output_root" "$target" 2>&1); then
        if ! printf '%s\n' "$output" | grep -Fq "CARGO_TARGET_DIR=\"$output_root\""; then
            fail "os/Makefile $target must pass KERNEL_OUTPUT_ROOT=$output_root to Cargo for $arch"
        elif ! printf '%s\n' "$output" | grep -Fq "$expected_elf"; then
            fail "os/Makefile $target must derive the $arch kernel ELF from KERNEL_OUTPUT_ROOT"
        elif ! printf '%s\n' "$output" | grep -Fq "$expected_bin"; then
            fail "os/Makefile $target must derive the $arch kernel binary from KERNEL_OUTPUT_ROOT"
        elif ! printf '%s\n' "$output" | grep -Fq "cp -f $expected_elf $alias"; then
            fail "os/Makefile $target must retain the stable $arch kernel alias"
        else
            pass "os/Makefile $target propagates KERNEL_OUTPUT_ROOT for $arch"
        fi
    else
        fail "os/Makefile $target must accept KERNEL_OUTPUT_ROOT for $arch"
    fi
}

require_kernel_output_root_probe rv64-kernel-build-only rv64 normal .contract-output/rv64 riscv64gc-unknown-none-elf ../kernel-rv
require_kernel_output_root_probe la64-kernel-build-only la64 normal .contract-output/la64 loongarch64-unknown-linux-gnu ../kernel-la
require_kernel_output_root_probe kernel rv64 normal .contract-output/facade-rv64 riscv64gc-unknown-none-elf ../kernel-rv
require_kernel_output_root_probe arch-build la64 normal .contract-output/facade-la64 loongarch64-unknown-linux-gnu ../kernel-la

exit "$overall"
