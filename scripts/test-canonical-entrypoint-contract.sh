#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

require_trace() {
    trace=$1
    expected=$2
    description=$3
    case "$trace" in
        *"$expected"*) printf 'PASS: %s\n' "$description" ;;
        *) fail "$description" ;;
    esac
}

for arch in rv64 la64; do
    case "$arch" in
        rv64)
            cpio_suffix=rv.cpio
            board=board_rvqemu
            target=riscv64gc-unknown-none-elf
            ;;
        la64)
            cpio_suffix=la.cpio
            board=board_laqemu
            target=loongarch64-unknown-linux-gnu
            ;;
    esac

    for profile in normal regression; do
        case "$profile" in
            normal) cpio="initramfs-$cpio_suffix" ;;
            regression) cpio="initramfs-regression-$cpio_suffix" ;;
        esac

        trace=$(make -C "$repo_root/os" -n -f "make/$arch.mk" \
            ktest-build-only MODE=release INITRAMFS_PROFILE="$profile" 2>&1) ||
            fail "$arch $profile ktest build-only dry-run must succeed"
        require_trace "$trace" "MANGO_INITRAMFS_CPIO=" "$arch $profile ktest receives the selected CPIO"
        require_trace "$trace" "MANGO_USER_OUTPUT_ROOT=" "$arch $profile ktest receives the selected user root"
        require_trace "$trace" "MANGO_USER_OUTPUT_MODE=\"release\"" "$arch $profile ktest receives the selected user mode"
        require_trace "$trace" "$cpio" "$arch $profile ktest selects its CPIO artifact"
        case "$trace" in
            *qemu-system-*) fail "$arch $profile ktest build-only dry-run must not launch QEMU" ;;
        esac

        run_trace=$(make -C "$repo_root/os" -n -f "make/$arch.mk" \
            ktest-run MODE=release INITRAMFS_PROFILE="$profile" 2>&1) ||
            fail "$arch $profile ktest-run dry-run must succeed"
        require_trace "$run_trace" "MANGO_INITRAMFS_CPIO=" "$arch $profile ktest-run inherits the selected CPIO"
        require_trace "$run_trace" "MANGO_USER_OUTPUT_ROOT=" "$arch $profile ktest-run inherits the selected user root"

        trace=$(make -C "$repo_root/os" -n ARCH="$arch" PROFILE="$profile" check 2>&1) ||
            fail "$arch $profile formal check dry-run must succeed"
        require_trace "$trace" "MANGO_INITRAMFS_CPIO=" "$arch $profile formal check receives the selected CPIO"
        require_trace "$trace" "MANGO_USER_OUTPUT_ROOT=" "$arch $profile formal check receives the selected user root"
        require_trace "$trace" "MANGO_USER_OUTPUT_MODE=\"release\"" "$arch $profile formal check receives the selected user mode"
        require_trace "$trace" "$cpio" "$arch $profile formal check selects its CPIO artifact"
        require_trace "$trace" "$board" "$arch $profile formal check selects a valid board feature"
        require_trace "$trace" "$target" "$arch $profile formal check selects its target"
    done
done

if make -C "$repo_root/os" -n ARCH=invalid PROFILE=normal check >/dev/null 2>&1; then
    fail 'formal check must reject an invalid architecture'
fi
printf 'PASS: formal check rejects an invalid architecture\n'

if make -C "$repo_root/os" -n ARCH=rv64 PROFILE=invalid check >/dev/null 2>&1; then
    fail 'formal check must reject an invalid profile'
fi
printf 'PASS: formal check rejects an invalid profile\n'

require_facade_rejection() {
    entry=$1
    target=$2
    description=$3
    shift 3
    if make -C "$entry" -n "$@" "$target" >/dev/null 2>&1; then
        fail "$target at $entry must reject $description"
    fi
    printf 'PASS: %s at %s rejects %s\n' "$target" "$entry" "$description"
}

for entry in "$repo_root" "$repo_root/os"; do
    for target in check ktest-build-only; do
        require_facade_rejection "$entry" "$target" 'missing ARCH and PROFILE'
        require_facade_rejection "$entry" "$target" 'missing PROFILE' ARCH=rv64
        require_facade_rejection "$entry" "$target" 'missing ARCH' PROFILE=normal
        require_facade_rejection "$entry" "$target" 'invalid ARCH' ARCH=invalid PROFILE=normal
        require_facade_rejection "$entry" "$target" 'invalid PROFILE' ARCH=rv64 PROFILE=invalid
        require_facade_rejection "$entry" "$target" 'multiple ARCH values' 'ARCH=rv64 la64' PROFILE=normal
        require_facade_rejection "$entry" "$target" 'multiple PROFILE values' ARCH=rv64 'PROFILE=normal regression'
    done
done
