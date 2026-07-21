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

user_recipe() {
    awk '
        /^[^[:space:]#][^:]*:/ {
            if (in_rule) exit
            if ($0 ~ /^user[[:space:]]*:/) in_rule = 1
            next
        }
        in_rule && /^[[:space:]]*\t/ { print }
    ' "$repo_root/Makefile"
}

if awk '
    /^[[:space:]]*\.PHONY[[:space:]]*:/ {
        declaration = $0
        sub(/^[^:]*:/, "", declaration)
        count = split(declaration, targets, /[[:space:]]+/)
        for (item = 1; item <= count; item++) if (targets[item] == "user") found = 1
    }
    END { exit(found ? 0 : 1) }
' "$repo_root/Makefile"; then
    pass 'root user target is phony'
else
    fail 'root user target must be phony'
fi

if awk '
    /^user[[:space:]]*:/ {
        found = 1
        line = $0
        sub(/^[^:]*:/, "", line)
        sub(/^[[:space:]]*/, "", line)
        valid = (line == "toolchain-preflight")
        exit
    }
    END { exit(found && valid ? 0 : 1) }
' "$repo_root/Makefile"; then
    pass 'root user retains only toolchain-preflight prerequisite'
else
    fail 'root user must retain only toolchain-preflight prerequisite'
fi

if [ "$(user_recipe)" = '	$(MAKE) -C os "ARCH=$(ARCH)" "PROFILE=$(PROFILE)" user' ]; then
    pass 'root user forwards quoted ARCH and PROFILE only to os user'
else
    fail 'root user must forward quoted ARCH and PROFILE only to os user'
fi

require_valid() {
    arch=$1
    if output=$(make -C "$repo_root" -n "ARCH=$arch" "PROFILE=normal" user 2>&1); then
        if ! printf '%s\n' "$output" | grep -Fq "make -C os \"ARCH=$arch\" \"PROFILE=normal\" user"; then
            fail "root user must forward ARCH=$arch PROFILE=normal to os"
        elif ! printf '%s\n' "$output" | grep -Fq "make ARCH=$arch -f make/$arch.mk user"; then
            fail "root user must delegate ARCH=$arch to make/$arch.mk user"
        else
            pass "root user accepts ARCH=$arch PROFILE=normal"
        fi
    else
        fail "root user must accept ARCH=$arch PROFILE=normal"
    fi
}

require_invalid() {
    description=$1
    shift
    if output=$(make -C "$repo_root" -n "$@" user 2>&1); then
        fail "root user must reject $description"
    elif printf '%s\n' "$output" | grep -Eq 'make/(rv64|la64)\.mk[[:space:]]+user'; then
        fail "root user must reject $description before arch user delegation"
    else
        pass "root user rejects $description before arch user delegation"
    fi
}

require_valid rv64
require_valid la64
require_invalid 'missing ARCH and PROFILE'
require_invalid 'missing PROFILE' 'ARCH=rv64'
require_invalid 'missing ARCH' 'PROFILE=normal'
require_invalid 'invalid ARCH' 'ARCH=arm64' 'PROFILE=normal'
require_invalid 'invalid PROFILE' 'ARCH=rv64' 'PROFILE=regression'
require_invalid 'multiple ARCH values' 'ARCH=rv64 la64' 'PROFILE=normal'
require_invalid 'multiple PROFILE values' 'ARCH=rv64' 'PROFILE=normal regression'

exit "$overall"
