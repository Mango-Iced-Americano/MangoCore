#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in /*) ;; *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;; esac

overall=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; overall=1; }

require_valid() {
    entry=$1 entry_dir=$2 arch=$3
    if output=$(make -C "$entry_dir" -n "ARCH=$arch" "PROFILE=normal" run 2>&1); then
        if printf '%s\n' "$output" | grep -Fq "make ARCH=$arch -f make/$arch.mk run INITRAMFS_PROFILE=normal"; then
            pass "$entry run accepts ARCH=$arch PROFILE=normal"
        else
            fail "$entry run must dispatch ARCH=$arch to its normal run target"
        fi
    else
        fail "$entry run must accept ARCH=$arch PROFILE=normal"
    fi
}

require_invalid() {
    entry=$1 entry_dir=$2 description=$3; shift 3
    if output=$(make -C "$entry_dir" -n "$@" run 2>&1); then
        fail "$entry run must reject $description"
    elif printf '%s\n' "$output" | grep -Eq 'make/(rv64|la64)\.mk[[:space:]]+run'; then
        fail "$entry run must reject $description before architecture run delegation"
    else
        pass "$entry run rejects $description before architecture run delegation"
    fi
}

require_direct_os_baseline() {
    if output=$(make -C "$repo_root/os" -n "ARCH=rv64" "PROFILE=normal" run 2>&1); then
        if printf '%s\n' "$output" | grep -Fq 'sh ../scripts/rustup-preflight.sh' \
            && ! printf '%s\n' "$output" | grep -Fq 'Welcome to MangoCore'; then
            pass "direct os run executes os/Makefile rather than a root os goal"
        else
            fail "direct os run must execute os/Makefile rather than a root os goal"
        fi
    else
        fail "direct os run must accept ARCH=rv64 PROFILE=normal"
    fi
}

require_root_setup_once() {
    arch=$1
    if ! output=$(make -C "$repo_root" -n "ARCH=$arch" "PROFILE=normal" run 2>&1); then
        fail "root run must accept ARCH=$arch PROFILE=normal"
        return
    fi

    for command in 'echo "Welcome to MangoCore Project Aspera🚀"' 'sh scripts/rustup-preflight.sh'; do
        count=$(printf '%s\n' "$output" | grep -Fc "$command" || true)
        if [ "$count" -ne 1 ]; then
            fail "root run must invoke $command exactly once for ARCH=$arch (got $count)"
        else
            pass "root run invokes $command exactly once for ARCH=$arch"
        fi
    done
}

require_root_parallel_invalid() {
    if output=$(make -C "$repo_root" -n -j8 "ARCH=bad" "PROFILE=normal" run 2>&1); then
        fail "root run must reject invalid ARCH under -j8"
    elif printf '%s\n' "$output" | grep -Eq 'Welcome to MangoCore|rustup-preflight|make/(rv64|la64)\.mk[[:space:]]+run'; then
        fail "root run must reject invalid ARCH under -j8 before setup or architecture run delegation"
    else
        pass "root run rejects invalid ARCH under -j8 before setup or architecture run delegation"
    fi
}

require_valid root "$repo_root" rv64
require_valid root "$repo_root" la64
require_valid os "$repo_root/os" rv64
require_valid os "$repo_root/os" la64
require_direct_os_baseline
require_root_setup_once rv64
require_root_setup_once la64
require_root_parallel_invalid

for entry in . os; do
    case "$entry" in
        .) entry_dir=$repo_root ;;
        os) entry_dir=$repo_root/os ;;
    esac
    require_invalid "$entry" "$entry_dir" 'missing ARCH and PROFILE'
    require_invalid "$entry" "$entry_dir" 'missing PROFILE' 'ARCH=rv64'
    require_invalid "$entry" "$entry_dir" 'missing ARCH' 'PROFILE=normal'
    require_invalid "$entry" "$entry_dir" 'invalid ARCH' 'ARCH=bad' 'PROFILE=normal'
    require_invalid "$entry" "$entry_dir" 'non-normal PROFILE' 'ARCH=rv64' 'PROFILE=regression'
    require_invalid "$entry" "$entry_dir" 'multiple ARCH values' 'ARCH=rv64 la64' 'PROFILE=normal'
    require_invalid "$entry" "$entry_dir" 'multiple PROFILE values' 'ARCH=rv64' 'PROFILE=normal extra'
done

for legacy in rv64-run la64-run; do
    if output=$(make -C "$repo_root/os" -n "$legacy" 2>&1) && printf '%s\n' "$output" | grep -Fq ' comp'; then
        pass "legacy $legacy retains comp dispatch"
    else
        fail "legacy $legacy must retain comp dispatch"
    fi
done

exit "$overall"
