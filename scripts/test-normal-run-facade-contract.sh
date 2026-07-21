#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in /*) ;; *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;; esac

overall=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; overall=1; }

require_valid() {
    entry=$1 arch=$2
    if output=$(make -C "$repo_root" -n $entry "ARCH=$arch" "PROFILE=normal" run 2>&1); then
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
    entry=$1 description=$2; shift 2
    if output=$(make -C "$repo_root" -n $entry "$@" run 2>&1); then
        fail "$entry run must reject $description"
    elif printf '%s\n' "$output" | grep -Eq 'make/(rv64|la64)\.mk[[:space:]]+run'; then
        fail "$entry run must reject $description before architecture run delegation"
    else
        pass "$entry run rejects $description before architecture run delegation"
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

require_valid . rv64
require_valid . la64
require_valid os rv64
require_valid os la64
require_root_setup_once rv64
require_root_setup_once la64
require_root_parallel_invalid

for entry in . os; do
    require_invalid "$entry" 'missing ARCH and PROFILE'
    require_invalid "$entry" 'missing PROFILE' 'ARCH=rv64'
    require_invalid "$entry" 'missing ARCH' 'PROFILE=normal'
    require_invalid "$entry" 'invalid ARCH' 'ARCH=bad' 'PROFILE=normal'
    require_invalid "$entry" 'non-normal PROFILE' 'ARCH=rv64' 'PROFILE=regression'
    require_invalid "$entry" 'multiple ARCH values' 'ARCH=rv64 la64' 'PROFILE=normal'
    require_invalid "$entry" 'multiple PROFILE values' 'ARCH=rv64' 'PROFILE=normal extra'
done

for legacy in rv64-run la64-run; do
    if output=$(make -C "$repo_root/os" -n "$legacy" 2>&1) && printf '%s\n' "$output" | grep -Fq ' comp'; then
        pass "legacy $legacy retains comp dispatch"
    else
        fail "legacy $legacy must retain comp dispatch"
    fi
done

exit "$overall"
