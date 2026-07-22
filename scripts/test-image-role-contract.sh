#!/bin/sh
# Static contract for the two-drive boot image ABI.  It deliberately never
# builds, mounts, downloads, or opens an external evaluator image for writing.
set -eu

usage() {
    printf '%s\n' "usage: $0 [--repo-root DIR] [--fixture NAME]" >&2
    exit 2
}

fail() {
    printf '%s\n' "FAIL: $*" >&2
    exit 1
}

role_value() {
    awk -v key="$1" '
        $0 ~ "^[[:space:]]*" key "[[:space:]]*(:)?=" {
            value = $0
            sub("^[[:space:]]*" key "[[:space:]]*(:)?=[[:space:]]*", "", value)
            sub(/^[[:space:]]*/, "", value)
            sub(/[[:space:]]*$/, "", value)
        }
        END { print value }
    ' "$2"
}

require_role() {
    value=$(role_value "$1" "$role_map")
    [ -n "$value" ] || fail "missing role-map value: $1"
}

require_line() {
    pattern=$1
    file=$2
    message=$3
    grep -F -- "$pattern" "$file" >/dev/null 2>&1 || fail "$message"
}

require_zero_drive_profile() {
    target=$1
    file=$2
    if awk -v target="$target" '
        $0 ~ "^" target ":[[:space:]]*" { active = 1; next }
        active && /^[^[:space:]#][^:]*:[^=]*$/ { active = 0 }
        active && /-drive|virtio-blk/ { bad = 1 }
        END { exit bad }
    ' "$file"; then
        return
    fi
    fail "$target must remain a zero-drive profile in $file"
}

check_repo() {
    root=$1
    role_map=$root/os/make/image-roles.mk
    rv_settings=$root/os/make/arch/rv64-settings.mk
    la_settings=$root/os/make/arch/la64-settings.mk
    rv_make=$root/os/make/rv64.mk
    la_make=$root/os/make/la64.mk
    tools_make=$root/os/make/tools-disk.mk
    inject_script=$root/os/inject_os_test_conf.sh

    [ -r "$role_map" ] || fail 'missing centralized image role map'
    for role in \
        IMAGE_ROLE_MANIFEST_VERSION \
        IMAGE_ROLE_DRIVE_ORDER \
        IMAGE_ROLE_RV64_BOOTSTRAP_ROOT \
        IMAGE_ROLE_LA64_BOOTSTRAP_ROOT \
        IMAGE_ROLE_RV64_DEVELOPMENT_X0 \
        IMAGE_ROLE_LA64_DEVELOPMENT_X0 \
        IMAGE_ROLE_RV64_COMPETITION_X0 \
        IMAGE_ROLE_LA64_COMPETITION_X0 \
        IMAGE_ROLE_RV64_X1 \
        IMAGE_ROLE_LA64_X1 \
        IMAGE_ROLE_X1_PARTITION1 \
        IMAGE_ROLE_X1_PARTITION2 \
        IMAGE_ROLE_X1_SCRATCH_DEVICE \
        IMAGE_ROLE_OFFICIAL_X0_MUTABLE; do
        require_role "$role"
    done

    [ "$(role_value IMAGE_ROLE_MANIFEST_VERSION "$role_map")" = '1' ] ||
        fail 'unsupported image role manifest version'
    [ "$(role_value IMAGE_ROLE_DRIVE_ORDER "$role_map")" = 'x0 x1' ] ||
        fail 'normal development ABI must be exactly x0 x1'
    [ "$(role_value IMAGE_ROLE_RV64_DEVELOPMENT_X0 "$role_map")" != "$(role_value IMAGE_ROLE_RV64_X1 "$role_map")" ] ||
        fail 'RV64 development x0 and x1 roles must not be swapped'
    [ "$(role_value IMAGE_ROLE_LA64_DEVELOPMENT_X0 "$role_map")" != "$(role_value IMAGE_ROLE_LA64_X1 "$role_map")" ] ||
        fail 'LA64 development x0 and x1 roles must not be swapped'
    [ "$(role_value IMAGE_ROLE_X1_PARTITION1 "$role_map")" = 'tools-ext4' ] ||
        fail 'x1 partition 1 must belong to tools'
    [ "$(role_value IMAGE_ROLE_X1_PARTITION2 "$role_map")" = 'scratch-fat32' ] ||
        fail 'x1 partition 2 must belong to scratch'
    [ "$(role_value IMAGE_ROLE_X1_SCRATCH_DEVICE "$role_map")" = '/dev/vdb2' ] ||
        fail 'scratch contract must document LTP_DEV=/dev/vdb2'
    [ "$(role_value IMAGE_ROLE_OFFICIAL_X0_MUTABLE "$role_map")" = 'no' ] ||
        fail 'official evaluator x0 must be immutable'

    require_line 'IMAGE_ROLE_RV64_COMPETITION_X0 := ../sdcard-rv.img' "$role_map" \
        'RV64 external evaluator sdcard name changed'
    require_line 'IMAGE_ROLE_LA64_COMPETITION_X0 := ../sdcard-la.img' "$role_map" \
        'LA64 external evaluator sdcard name changed'
    require_line 'include make/image-roles.mk' "$rv_make" 'RV64 QEMU must load the role map'
    require_line 'include make/image-roles.mk' "$la_make" 'LA64 QEMU must load the role map'
    require_line 'file=$(IMAGE_ROLE_RV64_DEVELOPMENT_X0),format=raw,id=x0' "$rv_make" \
        'RV64 development x0 must use the role map'
    require_line 'file=$(IMAGE_ROLE_RV64_X1),format=raw,id=x1' "$rv_make" \
        'RV64 development x1 must use the role map'
    require_line 'file=$(IMAGE_ROLE_RV64_COMPETITION_X0),if=none,format=raw,id=x0' "$rv_make" \
        'RV64 competition x0 must use the role map'
    require_line 'file=$(IMAGE_ROLE_LA64_DEVELOPMENT_X0),format=raw,id=x0' "$la_make" \
        'LA64 development x0 must use the role map'
    require_line 'file=$(IMAGE_ROLE_LA64_X1),format=raw,id=x1' "$la_make" \
        'LA64 development x1 must use the role map'
    require_line 'file=$(IMAGE_ROLE_LA64_COMPETITION_X0),if=none,format=raw,id=x0' "$la_make" \
        'LA64 competition x0 must use the role map'
    require_line 'IMAGE_ROLE_RV64_DEVELOPMENT_X0' "$rv_settings" \
        'RV64 development rootfs producer must consult the role map'
    require_line 'IMAGE_ROLE_LA64_DEVELOPMENT_X0' "$la_settings" \
        'LA64 development rootfs producer must consult the role map'

    if grep -E 'id=x[2-9]' "$rv_make" "$la_make" >/dev/null 2>&1; then
        fail 'permanent third QEMU disk is forbidden'
    fi
    for makefile in "$rv_make" "$la_make"; do
        require_zero_drive_profile regression-run "$makefile"
        require_zero_drive_profile ktest-run "$makefile"
    done

    require_line 'P1_MIB = 2048' "$root/scripts/make_mbr_tools_disk.py" \
        'tools disk P1 payload size changed without role-contract update'
    require_line 'P2_MIB = 1280' "$root/scripts/make_mbr_tools_disk.py" \
        'scratch P2 size changed without role-contract update'
    require_line 'put_partition_entry(mbr, 0, 0x83' "$root/scripts/make_mbr_tools_disk.py" \
        'tools disk P1 partition type changed'
    require_line 'put_partition_entry(mbr, 1, 0x0C' "$root/scripts/make_mbr_tools_disk.py" \
        'scratch P2 partition type changed'
    [ -r "$tools_make" ] || fail 'tools-disk builder must be split from oversized tools module'
    require_line 'mktemp -d' "$tools_make" 'tools disk workspace must be unique'
    if grep -F '/tmp/tools-mnt' "$tools_make" >/dev/null 2>&1; then
        fail 'tools disk builder must not use a global /tmp mountpoint'
    fi

    require_line 'official evaluator image is immutable' "$inject_script" \
        'config injection must reject official x0 mutation'
    require_line 'DERIVED_IMAGE_PATH' "$inject_script" \
        'config injection must require a named derived development image'
    if grep -E 'debugfs -w.*sdcard-(rv|la)\.img' "$inject_script" >/dev/null 2>&1; then
        fail 'config injection still writes an official sdcard directly'
    fi
    if grep -E 'debugfs -w.*sdcard-(rv|la)\.img|cp .*sdcard-(rv|la)\.img' "$root/os/Makefile" >/dev/null 2>&1; then
        fail 'legacy Make target still mutates an official evaluator image'
    fi

    printf '%s\n' 'PASS: image role contract'
}

run_fixture() {
    fixture=$1
    fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/image-role-contract.XXXXXX")
    trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
    repo=$fixture_root/repo
    mkdir -p "$repo/os/make/arch" "$repo/scripts"
    for file in \
        os/make/image-roles.mk \
        os/make/arch/rv64-settings.mk \
        os/make/arch/la64-settings.mk \
        os/make/rv64.mk \
        os/make/la64.mk \
        os/make/tools-disk.mk \
        os/inject_os_test_conf.sh \
        scripts/make_mbr_tools_disk.py; do
        mkdir -p "$repo/$(dirname "$file")"
        cp "$repo_root/$file" "$repo/$file"
    done

    case "$fixture" in
        swapped-drive)
            printf '%s\n' 'IMAGE_ROLE_DRIVE_ORDER := x1 x0' >>"$repo/os/make/image-roles.mk"
            ;;
        third-drive)
            printf '%s\n' '-drive if=none,file=forbidden.img,format=raw,id=x2' >>"$repo/os/make/rv64.mk"
            ;;
        missing-payload)
            printf '%s\n' 'IMAGE_ROLE_LA64_BOOTSTRAP_ROOT :=' >>"$repo/os/make/image-roles.mk"
            ;;
        mutate-official-x0)
            printf '%s\n' 'IMAGE_ROLE_OFFICIAL_X0_MUTABLE := yes' >>"$repo/os/make/image-roles.mk"
            ;;
        *) usage ;;
    esac

    if sh "$0" --repo-root "$repo" >"$fixture_root/output" 2>&1; then
        fail "fixture was accepted: $fixture"
    fi
    if ! grep -F 'FAIL:' "$fixture_root/output" >/dev/null; then
        cat "$fixture_root/output" >&2
        fail "fixture did not emit a contract failure: $fixture"
    fi
    printf '%s\n' "PASS: fixture rejected: $fixture"
}

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
case "$#" in
    0) check_repo "$repo_root" ;;
    2)
        case "$1" in
            --repo-root) check_repo "$2" ;;
            --fixture) run_fixture "$2" ;;
            *) usage ;;
        esac
        ;;
    *) usage ;;
esac
