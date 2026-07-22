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

assert_injection_guard() {
    destination=$1
    mkdir -p "$repo/build/development/rv64" "$repo/build/development/la64" "$fixture_root/bin"
    : >"$repo/sdcard-rv.img"
    sha256sum "$repo/sdcard-rv.img" >"$repo/sdcard-rv.img.sha256"
    : >"$repo/sdcard-la.img"
    sha256sum "$repo/sdcard-la.img" >"$repo/sdcard-la.img.sha256"
    printf '%s\n' 'mask=0x001' >"$fixture_root/os_test.conf"
    cat >"$fixture_root/bin/cp" <<'EOF'
#!/bin/sh
printf '%s\n' cp >>"$FIXTURE_MUTATION_LOG"
exit 99
EOF
    cat >"$fixture_root/bin/e2fsck" <<'EOF'
#!/bin/sh
printf '%s\n' e2fsck >>"$FIXTURE_MUTATION_LOG"
exit 99
EOF
    cat >"$fixture_root/bin/debugfs" <<'EOF'
#!/bin/sh
printf '%s\n' debugfs >>"$FIXTURE_MUTATION_LOG"
exit 99
EOF
    chmod +x "$fixture_root/bin/cp" "$fixture_root/bin/e2fsck" "$fixture_root/bin/debugfs"

    if PATH="$fixture_root/bin:$PATH" FIXTURE_MUTATION_LOG="$fixture_root/mutations" \
        ARCH=rv64 BLK_MODE=virt CONF_FILE="$fixture_root/os_test.conf" \
        DERIVED_IMAGE_PATH="$destination" "$repo/os/inject_os_test_conf.sh" >"$fixture_root/output" 2>&1; then
        fail "fixture was accepted: $fixture"
    fi
    grep -F 'image-role error:' "$fixture_root/output" >/dev/null || fail 'injection guard fixture lacked a diagnostic'
    [ ! -e "$fixture_root/mutations" ] || fail 'injection guard mutated an image before rejection'
}

check_repo() {
    root=$1
    role_map=$root/os/make/image-roles.mk
    rv_settings=$root/os/make/arch/rv64-settings.mk
    la_settings=$root/os/make/arch/la64-settings.mk
    rv_make=$root/os/make/rv64.mk
    la_make=$root/os/make/la64.mk
    qemu_profiles=$root/os/make/qemu-profiles.mk
    tools_make=$root/os/make/tools-disk.mk
    inject_script=$root/os/inject_os_test_conf.sh
    role_tool=$root/scripts/image_roles.py
    run_full=$root/scripts/full_test/commands.py
    run_full_runner=$root/scripts/full_test/runner.py
    auto_include=$root/scripts/auto_include_ltp.py
    auto_exclude=$root/scripts/auto_exclude_ltp.py

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
        IMAGE_ROLE_RV64_DERIVED_X0 \
        IMAGE_ROLE_LA64_DERIVED_X0 \
        IMAGE_ROLE_RV64_DERIVED_X0_NEXT \
        IMAGE_ROLE_LA64_DERIVED_X0_NEXT \
        IMAGE_ROLE_RV64_X1 \
        IMAGE_ROLE_LA64_X1 \
        IMAGE_ROLE_X1_PARTITION1 \
        IMAGE_ROLE_X1_PARTITION2 \
        IMAGE_ROLE_X1_SCRATCH_DEVICE \
        IMAGE_ROLE_OFFICIAL_X0_MUTABLE; do
        require_role "$role"
    done

    [ "$(role_value IMAGE_ROLE_MANIFEST_VERSION "$role_map")" = '2' ] ||
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
    [ -r "$qemu_profiles" ] || fail 'missing centralized QEMU profile arguments'
    require_line 'include make/qemu-profiles.mk' "$rv_make" 'RV64 QEMU must use centralized arguments'
    require_line 'include make/qemu-profiles.mk' "$la_make" 'LA64 QEMU must use centralized arguments'
    require_line 'define qemu_two_drives' "$qemu_profiles" 'central QEMU profile must define x0+x1 construction'
    require_line 'define qemu_zero_drives' "$qemu_profiles" 'central QEMU profile must define diskless construction'
    require_line 'qemu_profile_command,development' "$rv_make" \
        'RV64 development must use the canonical QEMU profile command'
    require_line 'qemu_profile_command,competition' "$rv_make" \
        'RV64 competition must use the canonical QEMU profile command'
    require_line 'qemu_profile_command,derived-competition' "$rv_make" \
        'RV64 derived-competition must use the canonical QEMU profile command'
    require_line 'qemu_profile_command,development' "$la_make" \
        'LA64 development must use the canonical QEMU profile command'
    require_line 'qemu_profile_command,competition' "$la_make" \
        'LA64 competition must use the canonical QEMU profile command'
    require_line 'qemu_profile_command,derived-competition' "$la_make" \
        'LA64 derived-competition must use the canonical QEMU profile command'
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

    require_line 'DERIVED_IMAGE_PATH' "$inject_script" \
        'config injection must require a named derived development image'
    require_line 'validate-derived' "$inject_script" \
        'config injection must validate the derived output before mutation'
    require_line 'validate-mutable' "$inject_script" \
        'config injection must reject every official alias before fsck/debugfs'
    if grep -E 'debugfs -w.*sdcard-(rv|la)\.img' "$inject_script" >/dev/null 2>&1; then
        fail 'config injection still writes an official sdcard directly'
    fi
    if grep -E 'debugfs -w.*sdcard-(rv|la)\.img|cp .*sdcard-(rv|la)\.img' "$root/os/Makefile" >/dev/null 2>&1; then
        fail 'legacy Make target still mutates an official evaluator image'
    fi
    [ -r "$role_tool" ] || fail 'Python consumers need the image-role interface'
    for consumer in "$run_full" "$auto_include" "$auto_exclude"; do
        require_line 'from image_roles import' "$consumer" 'Python consumer bypasses image-role interface'
    done
    require_line 'from image_roles import' "$run_full" 'full-test commands must load the image-role interface'
    require_line 'qemu-profile-dry-run' "$run_full" 'full-test QEMU commands must delegate to the Make profile renderer'
    require_line 'validate_official' "$run_full_runner" 'full-test must validate official archives before extraction'
    for consumer in "$auto_include" "$auto_exclude"; do
        require_line 'derived-run' "$consumer" 'LTP must run named derived-image QEMU targets'
        require_line 'CONF_IMAGE=' "$consumer" 'LTP injection must name its derived x0 target'
        require_line 'validate_official' "$consumer" 'LTP must validate official archives before extraction'
    done
    if grep -E 'sdcard-(rv|la)\.img|disk(-la)?\.img' "$run_full" "$run_full_runner" "$auto_include" "$auto_exclude" >/dev/null 2>&1; then
        fail 'a Python image consumer still hard-codes an image role'
    fi
    require_line 'workspace=$$(mktemp -d' "$tools_make" 'tools workspace must fail fast when mktemp fails'
    require_line 'tools workspace creation failed' "$tools_make" 'tools mktemp failure must preserve a diagnostic'
    require_line 'preserving $$workspace' "$tools_make" 'tools unmount failure must preserve diagnostics and workspace'

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
        os/make/qemu-profiles.mk \
        os/make/common/toolchain.mk \
        os/make/tools-disk.mk \
        os/inject_os_test_conf.sh \
        scripts/image_roles.py \
        scripts/run_full_test.py \
        scripts/full_test/__init__.py \
        scripts/full_test/commands.py \
        scripts/full_test/runner.py \
        scripts/full_test/cli.py \
        scripts/auto_include_ltp.py \
        scripts/auto_exclude_ltp.py \
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
        remaining-consumer)
            printf '%s\n' '"-drive file=sdcard-rv.img,if=none,format=raw,id=x0 "' >>"$repo/scripts/run_full_test.py"
            ;;
        cross-arch-derived)
            assert_injection_guard "$repo/build/development/la64/sdcard-la.img"
            printf '%s\n' "PASS: fixture rejected: $fixture"
            return
            ;;
        symlink-alias|hardlink-alias)
            mkdir -p "$repo/build/development/rv64"
            : >"$repo/sdcard-rv.img"
            case "$fixture" in
                symlink-alias) ln -s "$repo/sdcard-rv.img" "$repo/build/development/rv64/sdcard-rv-derived.img" ;;
                hardlink-alias) ln "$repo/sdcard-rv.img" "$repo/build/development/rv64/sdcard-rv-derived.img" ;;
            esac
            assert_injection_guard "$repo/build/development/rv64/sdcard-rv-derived.img"
            printf '%s\n' "PASS: fixture rejected: $fixture"
            return
            ;;
        basename-alias)
            mkdir -p "$repo/build/development/rv64"
            : >"$repo/sdcard-rv.img"
            if python3 "$repo/scripts/image_roles.py" validate-mutable --repo-root "$repo" --arch rv64 --path "$repo/build/development/rv64/sdcard-rv.img" >"$fixture_root/output" 2>&1; then
                fail "fixture was accepted: $fixture"
            fi
            grep -F 'image-role error:' "$fixture_root/output" >/dev/null || fail 'alias fixture lacked a diagnostic'
            printf '%s\n' "PASS: fixture rejected: $fixture"
            return
            ;;
        make-override)
            if make -n -C "$repo/os" -f make/rv64.mk check-development-x0 IMAGE_ROLE_RV64_DEVELOPMENT_X0=../sdcard-rv.img >"$fixture_root/output" 2>&1; then
                fail "fixture was accepted: $fixture"
            fi
            grep -F 'IMAGE_ROLE_RV64_DEVELOPMENT_X0 resolves to an immutable official x0' "$fixture_root/output" >/dev/null || fail 'Make override fixture lacked a diagnostic'
            printf '%s\n' "PASS: fixture rejected: $fixture"
            return
            ;;
        mktemp-failure|unmount-failure)
            mkdir -p "$repo/empty" "$fixture_root/bin"
cat >"$repo/fixture.mk" <<'EOF'
CPYTHON_COMMON := $(CURDIR)/missing-cpython
include os/make/tools-disk.mk
all:
	$(call build_tools_disk,$(CURDIR)/out.img,1,$(CURDIR)/empty,rv)
EOF
            cat >"$fixture_root/bin/mktemp" <<'EOF'
#!/bin/sh
exit 1
EOF
            chmod +x "$fixture_root/bin/mktemp"
            if [ "$fixture" = unmount-failure ]; then
                cat >"$fixture_root/bin/mktemp" <<'EOF'
#!/bin/sh
mkdir -p "${TMPDIR:-/tmp}/fixture-workspace"
printf '%s\n' "${TMPDIR:-/tmp}/fixture-workspace"
EOF
                cat >"$fixture_root/bin/dd" <<'EOF'
#!/bin/sh
for arg in "$@"; do case "$arg" in of=*) : >"${arg#of=}" ;; esac; done
EOF
                cat >"$fixture_root/bin/mkfs.ext4" <<'EOF'
#!/bin/sh
exit 0
EOF
                cat >"$fixture_root/bin/mount" <<'EOF'
#!/bin/sh
for last; do :; done
mkdir -p "$last/lib" "$last/bin" "$last/tests"
EOF
                cat >"$fixture_root/bin/umount" <<'EOF'
#!/bin/sh
printf '%s\n' 'fixture umount failure' >&2
exit 1
EOF
                chmod +x "$fixture_root/bin/mktemp" "$fixture_root/bin/dd" "$fixture_root/bin/mkfs.ext4" "$fixture_root/bin/mount" "$fixture_root/bin/umount"
            fi
            status=0
            PATH="$fixture_root/bin:$PATH" TMPDIR="$fixture_root" timeout 5 make -C "$repo" -f fixture.mk all >"$fixture_root/output" 2>&1 || status=$?
            if [ "$status" -eq 0 ]; then
                fail "fixture was accepted: $fixture"
            fi
            if [ "$status" -eq 124 ]; then
                cat "$fixture_root/output" >&2
                fail "$fixture fixture did not fail fast"
            fi
            expected='tools workspace creation failed'
            [ "$fixture" = unmount-failure ] && expected='tools workspace unmount failed; preserving'
            if ! grep -F "$expected" "$fixture_root/output" >/dev/null; then
                cat "$fixture_root/output" >&2
                fail "$fixture fixture lacked a diagnostic"
            fi
            printf '%s\n' "PASS: fixture rejected: $fixture"
            return
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
