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

target_recipe() {
    awk '
        /^[^[:space:]#][^:]*:/ {
            if (in_rule) {
                exit
            }
            if ($0 ~ /^kernel[[:space:]]*:/) {
                in_rule = 1
            }
            next
        }
        in_rule && /^[[:space:]]*\t/ { print }
    ' "$repo_root/Makefile"
}

if awk '
    /^kernel[[:space:]]*:/ {
        found = 1
        line = $0
        sub(/^[^:]*:/, "", line)
        sub(/^[[:space:]]*/, "", line)
        valid = (line == "toolchain-preflight")
        exit
    }
    END { exit(found && valid ? 0 : 1) }
' "$repo_root/Makefile"; then
    pass 'root kernel retains only toolchain-preflight prerequisite'
else
    fail 'root kernel must retain only toolchain-preflight prerequisite'
fi

if [ "$(target_recipe)" = '	$(MAKE) -C os "ARCH=$(ARCH)" "PROFILE=$(PROFILE)" kernel' ]; then
    pass 'root kernel forwards quoted ARCH and PROFILE only to os kernel'
else
    fail 'root kernel must forward quoted ARCH and PROFILE only to os kernel'
fi

require_valid() {
    arch=$1
    profile=$2
    expected_arch_make=$3
    if output=$(make -C "$repo_root" -n "ARCH=$arch" "PROFILE=$profile" kernel 2>&1); then
        if ! printf '%s\n' "$output" | grep -Fq "make -C os \"ARCH=$arch\" \"PROFILE=$profile\" kernel"; then
            fail "root kernel must forward ARCH=$arch PROFILE=$profile to os"
        elif ! printf '%s\n' "$output" | grep -Fq "make ARCH=$arch -f make/$expected_arch_make.mk build"; then
            fail "root kernel must delegate ARCH=$arch to make/$expected_arch_make.mk"
        elif ! printf '%s\n' "$output" | grep -Fq "INITRAMFS_PROFILE=$profile"; then
            fail "root kernel must retain PROFILE=$profile through the os facade"
        else
            pass "root kernel accepts ARCH=$arch PROFILE=$profile"
        fi
    else
        fail "root kernel must accept ARCH=$arch PROFILE=$profile"
    fi
}

require_invalid() {
    description=$1
    shift
    if output=$(make -C "$repo_root" -n "$@" kernel 2>&1); then
        fail "root kernel must reject $description"
    elif printf '%s\n' "$output" | grep -Eq 'make/(rv64|la64)\.mk[[:space:]]+build'; then
        fail "root kernel must reject $description before arch build delegation"
    else
        pass "root kernel rejects $description before arch build delegation"
    fi
}

require_valid rv64 normal rv64
require_valid rv64 regression rv64
require_valid la64 normal la64
require_valid la64 regression la64

require_invalid 'missing ARCH and PROFILE'
require_invalid 'missing PROFILE' 'ARCH=rv64'
require_invalid 'missing ARCH' 'PROFILE=normal'
require_invalid 'invalid ARCH' 'ARCH=arm64' 'PROFILE=normal'
require_invalid 'invalid PROFILE' 'ARCH=rv64' 'PROFILE=staging'
require_invalid 'multiple ARCH values' 'ARCH=rv64 la64' 'PROFILE=normal'
require_invalid 'multiple PROFILE values' 'ARCH=rv64' 'PROFILE=normal regression'

exit "$overall"
