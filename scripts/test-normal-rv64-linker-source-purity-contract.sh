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

build_probe=$(mktemp)
trap 'rm -f "$root_stanza" "$os_stanza" "$build_probe"' 0

if rustc "$BUILD_RS" -o "$build_probe"; then
    run_valid_feature_probe() {
        board=$1
        feature=$2
        linker=$3
        if output=$(env CARGO_CFG_TARGET_ARCH=riscv64 "$feature"=1 "$build_probe" 2>&1); then
            case "$output" in
                *"cargo:rerun-if-changed=$linker"*"cargo:rustc-link-arg=-T$linker"*)
                    printf '%s\n' "PASS: RV64 $board-only feature selects $linker"
                    ;;
                *)
                    fail "RV64 $board-only feature must select $linker"
                    ;;
            esac
        else
            fail "RV64 $board-only feature probe must succeed"
        fi
    }

    run_invalid_feature_probe() {
        fixture=$1
        shift
        if output=$(env CARGO_CFG_TARGET_ARCH=riscv64 "$@" "$build_probe" 2>&1); then
            fail "$fixture fixture must reject an invalid RV64 board feature set"
            return
        fi
        case "$output" in
            *"RV64 build requires exactly one board feature"*)
                fail "$fixture fixture confirmed rejection of invalid RV64 board feature set"
                ;;
            *)
                fail "$fixture fixture must report the RV64 board-feature invariant"
                ;;
        esac
    }

    case "${1:-}" in
        '')
            run_valid_feature_probe rvqemu CARGO_FEATURE_BOARD_RVQEMU src/hal/arch/riscv/linker-rvqemu.ld
            run_valid_feature_probe vf2 CARGO_FEATURE_BOARD_VF2 src/hal/arch/riscv/linker-vf2.ld
            ;;
        --fixture)
            case "${2:-}" in
                no-board)
                    run_invalid_feature_probe no-board
                    ;;
                dual-board)
                    run_invalid_feature_probe dual-board CARGO_FEATURE_BOARD_RVQEMU=1 CARGO_FEATURE_BOARD_VF2=1
                    ;;
                *)
                    printf '%s\n' 'usage: test-normal-rv64-linker-source-purity-contract.sh [--fixture no-board|dual-board]' >&2
                    exit 2
                    ;;
            esac
            exit 1
            ;;
        *)
            printf '%s\n' 'usage: test-normal-rv64-linker-source-purity-contract.sh [--fixture no-board|dual-board]' >&2
            exit 2
            ;;
    esac
else
    fail 'cannot compile os/build.rs for RV64 linker feature probes'
fi

if [ "$failures" -ne 0 ]; then
    printf '%s\n' "normal RV64 linker source purity contract: RED ($failures failure(s))" >&2
    exit 1
fi

printf '%s\n' "normal RV64 linker source purity contract: PASS"
