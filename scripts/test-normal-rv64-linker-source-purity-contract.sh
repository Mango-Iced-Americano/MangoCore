#!/bin/sh

set -u

ROOT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
RV64_MAKEFILE=$ROOT_DIR/os/make/rv64.mk
BUILD_RS=$ROOT_DIR/os/build.rs
ROOT_CARGO_CONFIG=$ROOT_DIR/cargo-config/os/config.toml
OS_CARGO_CONFIG=$ROOT_DIR/os/.cargo/config.toml

failures=0

fail() {
    printf '%s\n' "FAIL: $1" >&2
    failures=$((failures + 1))
}

if [ ! -f "$RV64_MAKEFILE" ]; then
    fail "missing os/make/rv64.mk"
else
    recipe_count=$(awk '
        /^[[:space:]]*kernel:[[:space:]]+\$\(LWEXT4_PREREQ\)[[:space:]]*$/ { count++ }
        END { print count + 0 }
    ' "$RV64_MAKEFILE")
    if [ "$recipe_count" -ne 1 ]; then
        fail "expected exactly one normal RV64 kernel: \$(LWEXT4_PREREQ) recipe, found $recipe_count"
    fi

    if awk '
        /^[[:space:]]*kernel:[[:space:]]+\$\(LWEXT4_PREREQ\)[[:space:]]*$/ { in_recipe=1; next }
        in_recipe && /^[^[:space:]]/ { in_recipe=0 }
        in_recipe && /cp[[:space:]]+(-f[[:space:]]+)?src\/hal\/arch\/riscv\/linker-\$\(BOARD\)\.ld[[:space:]]+src\/hal\/arch\/riscv\/linker\.ld/ { found=1 }
        END { exit found ? 0 : 1 }
    ' "$RV64_MAKEFILE"; then
        fail "normal RV64 kernel recipe copies into tracked src/hal/arch/riscv/linker.ld"
    fi
fi

check_rv64_config() {
    config=$1
    if [ ! -f "$config" ]; then
        fail "missing $config"
        return
    fi

    if awk '
        /^\[target\.riscv64gc-unknown-none-elf\][[:space:]]*$/ { in_rv=1; next }
        in_rv && /^\[/ { in_rv=0 }
        in_rv && /-Clink-arg=-Tsrc\/hal\/arch\/riscv\/linker\.ld/ { found=1 }
        END { exit found ? 0 : 1 }
    ' "$config"; then
        fail "$config must not keep active RV64 linker -Tsrc/hal/arch/riscv/linker.ld"
    fi
}

check_rv64_config "$ROOT_CARGO_CONFIG"
check_rv64_config "$OS_CARGO_CONFIG"

if [ ! -f "$BUILD_RS" ]; then
    fail "missing os/build.rs"
else
    if ! awk '/linker-rvqemu\.ld/ { found=1 } END { exit found ? 0 : 1 }' "$BUILD_RS"; then
        fail "os/build.rs must contain linker-rvqemu.ld"
    fi
    if ! awk '/linker-vf2\.ld/ { found=1 } END { exit found ? 0 : 1 }' "$BUILD_RS"; then
        fail "os/build.rs must contain linker-vf2.ld"
    fi
    if ! awk '/cargo:rerun-if-changed=.*linker-rvqemu\.ld/ { found=1 } END { exit found ? 0 : 1 }' "$BUILD_RS"; then
        fail "os/build.rs must rerun when linker-rvqemu.ld changes"
    fi
    if ! awk '/cargo:rerun-if-changed=.*linker-vf2\.ld/ { found=1 } END { exit found ? 0 : 1 }' "$BUILD_RS"; then
        fail "os/build.rs must rerun when linker-vf2.ld changes"
    fi
    if ! awk '/cargo:rustc-link-arg(-[^=[:space:]]+)?=/ { found=1 } END { exit found ? 0 : 1 }' "$BUILD_RS"; then
        fail "os/build.rs must emit a Cargo linker argument directive"
    fi
fi

root_stanza=$(mktemp)
os_stanza=$(mktemp)
trap 'rm -f "$root_stanza" "$os_stanza"' 0

extract_rv64_stanza() {
    awk '
        /^\[target\.riscv64gc-unknown-none-elf\][[:space:]]*$/ { in_rv=1; print; next }
        in_rv && /^\[/ { exit }
        in_rv { print }
    ' "$1"
}

if [ -f "$ROOT_CARGO_CONFIG" ] && [ -f "$OS_CARGO_CONFIG" ]; then
    extract_rv64_stanza "$ROOT_CARGO_CONFIG" >"$root_stanza"
    extract_rv64_stanza "$OS_CARGO_CONFIG" >"$os_stanza"
    if ! diff -u "$root_stanza" "$os_stanza" >/dev/null 2>&1; then
        fail "RV64 target stanzas differ between cargo-config/os/config.toml and os/.cargo/config.toml"
    fi
fi

if [ "$failures" -ne 0 ]; then
    printf '%s\n' "normal RV64 linker source purity contract: RED ($failures failure(s))" >&2
    exit 1
fi

printf '%s\n' "normal RV64 linker source purity contract: PASS"
