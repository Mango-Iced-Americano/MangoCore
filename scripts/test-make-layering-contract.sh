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

require_file() {
    file=$1
    if [ -r "$repo_root/$file" ]; then
        pass "future layer module exists: $file"
        return 0
    fi
    fail "future layer module is not present: $file"
    return 1
}

require_include() {
    file=$1
    included=$2
    if grep -Eq "^[[:space:]]*-?include[[:space:]]+[^#]*${included}([[:space:]]|$)" "$repo_root/$file"; then
        pass "$file includes $included"
    else
        fail "$file must include $included"
    fi
}

require_target() {
    file=$1
    target=$2
    if awk -v target="$target" '
        /^[[:space:]]*define([[:space:]]|$)/ { in_define = 1; next }
        /^[[:space:]]*endef([[:space:]]|$)/ { in_define = 0; next }
        in_define { next }
        /^[^[:space:]#][^:]*:/ {
            split($0, parts, ":")
            count = split(parts[1], targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target) {
                    found = 1
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"; then
        pass "$file retains legacy target $target"
    else
        fail "$file must retain legacy target $target"
    fi
}

require_prerequisite() {
    file=$1
    target=$2
    prerequisite=$3
    if awk -v target="$target" -v prerequisite="$prerequisite" '
        /^[[:space:]]*define([[:space:]]|$)/ { in_define = 1; next }
        /^[[:space:]]*endef([[:space:]]|$)/ { in_define = 0; next }
        in_define { next }
        /^[^[:space:]#][^:]*:/ {
            split($0, parts, ":")
            count = split(parts[1], targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target && $0 ~ ("(^|[[:space:]])" prerequisite "([[:space:]]|$)")) {
                    found = 1
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"; then
        pass "$file $target retains prerequisite $prerequisite"
    else
        fail "$file $target must retain prerequisite $prerequisite"
    fi
}

require_exact_prerequisites() {
    file=$1
    target=$2
    prerequisites=$3
    if awk -v target="$target" -v prerequisites="$prerequisites" '
        /^[[:space:]]*define([[:space:]]|$)/ { in_define = 1; next }
        /^[[:space:]]*endef([[:space:]]|$)/ { in_define = 0; next }
        in_define { next }
        /^[^[:space:]#][^:]*:/ {
            split($0, parts, ":")
            count = split(parts[1], targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target) {
                    actual = parts[2]
                    sub(/^[[:space:]]*/, "", actual)
                    sub(/[[:space:]]*$/, "", actual)
                    if (actual == prerequisites) {
                        found = 1
                    }
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"; then
        pass "$file $target has exactly these prerequisites: $prerequisites"
    else
        fail "$file $target must have exactly these prerequisites: $prerequisites"
    fi
}

target_exists() {
    file=$1
    target=$2
    awk -v target="$target" '
        /^[[:space:]]*define([[:space:]]|$)/ { in_define = 1; next }
        /^[[:space:]]*endef([[:space:]]|$)/ { in_define = 0; next }
        in_define { next }
        /^[^[:space:]#][^:]*:/ {
            split($0, parts, ":")
            count = split(parts[1], targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target) {
                    found = 1
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"
}

require_phony_target() {
    file=$1
    target=$2
    if awk -v target="$target" '
        /^[[:space:]]*\.PHONY[[:space:]]*:/ {
            declaration = $0
            sub(/^[^:]*:/, "", declaration)
            count = split(declaration, targets, /[[:space:]]+/)
            for (item = 1; item <= count; item++) {
                if (targets[item] == target) {
                    found = 1
                }
            }
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/$file"; then
        pass "$file declares .PHONY facade $target"
    else
        fail "$file must declare .PHONY facade $target"
    fi
}

target_body() {
    file=$1
    target=$2
    awk -v target="$target" '
        /^[[:space:]]*define([[:space:]]|$)/ { in_define = 1; next }
        /^[[:space:]]*endef([[:space:]]|$)/ { in_define = 0; next }
        in_define { next }
        /^[^[:space:]#.][^:]*:/ {
            if (in_rule) {
                exit
            }
            if ($0 ~ ("(^|[[:space:]])" target "([[:space:]]|:)")) {
                in_rule = 1
            }
            next
        }
        in_rule { print }
    ' "$repo_root/$file"
}

require_body_match() {
    file=$1
    target=$2
    description=$3
    pattern=$4
    if target_body "$file" "$target" | grep -Eq "$pattern"; then
        pass "$file $target $description"
    else
        fail "$file $target $description"
    fi
}

require_body_absent() {
    file=$1
    target=$2
    description=$3
    pattern=$4
    if target_body "$file" "$target" | grep -Eiq "$pattern"; then
        fail "$file $target $description"
    else
        pass "$file $target $description"
    fi
}

require_declaration_only() {
    file=$1
    violations=$(awk '
        continued {
            continued = ($0 ~ /\\[[:space:]]*$/)
            next
        }
        /^[[:space:]]*#/ || /^[[:space:]]*$/ { next }
        /^[[:space:]]*(ifeq|ifneq|ifdef|ifndef|else|endif)([[:space:]]|$)/ { next }
        /^[[:space:]]*(-?include|sinclude)[[:space:]]+/ { next }
        /^[[:space:]]*(export[[:space:]]+)?[^[:space:]:=+?][^:=+?]*[[:space:]]*(:=|[?]=|[+]=|=)/ {
            continued = ($0 ~ /\\[[:space:]]*$/)
            next
        }
        /^[[:space:]]*(export|unexport)([[:space:]]|$)/ { next }
        /^[[:space:]]*\t/ { print NR ": tab recipe: " $0; next }
        /^[[:space:]]*[^[:space:]#][^:=]*:/ { print NR ": target rule: " $0; next }
        { print NR ": non-declaration: " $0 }
    ' "$repo_root/$file")
    if [ -n "$violations" ]; then
        fail "$file must contain declarations/includes only"
        printf '%s\n' "$violations" >&2
    else
        pass "$file is declaration-only"
    fi
}

make_dry_run() {
    target=$1
    arch=$2
    profile=$3
    make -C "$repo_root/os" -n "ARCH=$arch" "PROFILE=$profile" "$target"
}

require_valid_facade_probe() {
    target=$1
    arch=$2
    profile=$3
    legacy=$4
    if output=$(make_dry_run "$target" "$arch" "$profile" 2>&1); then
        if printf '%s\n' "$output" | grep -Eq "make[[:space:]]+ARCH=${arch}[[:space:]]+-f[[:space:]]+make/${arch}\\.mk[[:space:]]+${legacy}([[:space:]]|$)"; then
            pass "os/Makefile $target accepts ARCH=$arch PROFILE=$profile and delegates to $legacy"
        else
            fail "os/Makefile $target must delegate valid ARCH=$arch PROFILE=$profile to $legacy"
        fi
    else
        fail "os/Makefile $target must accept ARCH=$arch PROFILE=$profile"
    fi
}

require_invalid_facade_probe() {
    target=$1
    arch=$2
    profile=$3
    legacy=$4
    if output=$(make_dry_run "$target" "$arch" "$profile" 2>&1); then
        fail "os/Makefile $target must reject ARCH=$arch PROFILE=$profile"
    elif printf '%s\n' "$output" | grep -Eq "make[[:space:]]+ARCH=.*[[:space:]]+-f[[:space:]]+make/.*\\.mk[[:space:]]+${legacy}([[:space:]]|$)"; then
        fail "os/Makefile $target must reject ARCH=$arch PROFILE=$profile before $legacy delegation"
    else
        pass "os/Makefile $target rejects ARCH=$arch PROFILE=$profile before $legacy delegation"
    fi
}

common_toolchain=os/make/common/toolchain.mk
common_orchestration=os/make/common/orchestration.mk
rv_settings=os/make/arch/rv64-settings.mk
la_settings=os/make/arch/la64-settings.mk

for module in "$common_toolchain" "$common_orchestration" "$rv_settings" "$la_settings"; do
    if require_file "$module"; then
        require_declaration_only "$module"
    fi
done

require_include os/Makefile make/common/toolchain.mk
require_include os/Makefile make/common/orchestration.mk
require_include os/make/rv64.mk make/common/toolchain.mk
require_include os/make/rv64.mk make/arch/rv64-settings.mk
require_include os/make/la64.mk make/common/toolchain.mk
require_include os/make/la64.mk make/arch/la64-settings.mk

tools_module=os/make/tools.mk
if require_file "$tools_module"; then
    :
fi

tools_include_count=$(awk '
    /^[[:space:]]*-?include[[:space:]]+make\/tools\.mk([[:space:]]|$)/ { count++ }
    END { print count + 0 }
' "$repo_root/os/Makefile")
if [ "$tools_include_count" -eq 1 ] && awk '
    /^[[:space:]]*all[[:space:]]*:/ { all_line = NR }
    /^[[:space:]]*-?include[[:space:]]+make\/tools\.mk([[:space:]]|$)/ && NR > all_line { found = 1 }
    END { exit(found ? 0 : 1) }
' "$repo_root/os/Makefile"; then
    pass 'os/Makefile includes make/tools.mk exactly once after all'
else
    fail 'os/Makefile must include make/tools.mk exactly once after all'
fi

initramfs_module=os/make/initramfs.mk
initramfs_include_count=$(awk '
    /^[[:space:]]*-?include[[:space:]]+make\/initramfs\.mk([[:space:]]|$)/ { count++ }
    END { print count + 0 }
' "$repo_root/os/Makefile")
if [ "$initramfs_include_count" -eq 1 ] && awk '
    /^[[:space:]]*-?include[[:space:]]+make\/tools\.mk([[:space:]]|$)/ { tools_line = NR }
    /^[[:space:]]*-?include[[:space:]]+make\/initramfs\.mk([[:space:]]|$)/ && NR > tools_line { found = 1 }
    END { exit(found ? 0 : 1) }
' "$repo_root/os/Makefile"; then
    pass 'os/Makefile includes make/initramfs.mk exactly once after make/tools.mk'
else
    fail 'os/Makefile must include make/initramfs.mk exactly once after make/tools.mk'
fi

if require_file "$initramfs_module"; then
    initramfs_targets_present=1
    for target in initramfs-rv initramfs-la initramfs-all; do
        if target_exists "$initramfs_module" "$target"; then
            pass "$initramfs_module owns target $target"
        else
            fail "$initramfs_module must own target $target"
            initramfs_targets_present=0
        fi
    done

    if [ "$initramfs_targets_present" -eq 1 ]; then
        require_exact_prerequisites "$initramfs_module" initramfs-rv user
        require_exact_prerequisites "$initramfs_module" initramfs-la user
        require_exact_prerequisites "$initramfs_module" initramfs-all 'initramfs-rv initramfs-la'
        require_body_match "$initramfs_module" initramfs-rv \
            'invokes the RV64 initramfs builder with the owned directory' \
            'build_initramfs\.sh[[:space:]]+rv64[[:space:]]+\$\(MODE\)[[:space:]]+\$\(INITRAMFS_DIR_RV\)'
        require_body_match "$initramfs_module" initramfs-la \
            'invokes the LA64 initramfs builder with the owned directory' \
            'build_initramfs\.sh[[:space:]]+la64[[:space:]]+\$\(MODE\)[[:space:]]+\$\(INITRAMFS_DIR_LA\)'
        for target in initramfs-rv initramfs-la initramfs-all; do
            require_body_absent "$initramfs_module" "$target" \
                'must not copy or touch lang_items sources' \
                '(^|[;&|[:space:]])@?(cp|touch)[[:space:]][^#]*lang_items\.rs'
            if target_exists os/Makefile "$target"; then
                fail "os/Makefile must not own target $target after initramfs module extraction"
            else
                pass "os/Makefile no longer owns target $target"
            fi
        done
    fi
else
    fail "$initramfs_module must own initramfs-rv, initramfs-la, and initramfs-all"
fi

if awk '
    /^[[:space:]]*#/ || /^[[:space:]]*$/ || /^[[:space:]]*-?include[[:space:]]+/ { next }
    /^[[:space:]]*\./ || /^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*[[:space:]]*[:?+]?=/ { next }
    /^[^[:space:]#.][^:]*:/ {
        split($0, parts, ":")
        split(parts[1], targets, /[[:space:]]+/)
        exit(targets[1] == "all" ? 0 : 1)
    }
    END { if (!NR) exit 1 }
' "$repo_root/os/Makefile"; then
    pass 'os/Makefile retains all as the default goal'
else
    fail 'os/Makefile must retain all as the default goal'
fi

if grep -Eq '^[[:space:]]*\.NOTPARALLEL[[:space:]]*:' "$repo_root/os/Makefile"; then
    pass 'os/Makefile retains .NOTPARALLEL'
else
    fail 'os/Makefile must retain .NOTPARALLEL'
fi

require_prerequisite os/Makefile rv64_all tools-disk-rv
require_prerequisite os/Makefile la64_all tools-disk-la

for target in \
    tools-disk-rv tools-disk-la tools-disk \
    tools-alpine-rv tools-alpine-la tools-alpine \
    tools-cpython-rv tools-cpython-la tools-cpython \
    maybe-tools-cpython-rv maybe-tools-cpython-la \
    tools-cpython-clean \
    tools-apk-rv tools-apk-la tools-apk; do
    require_target "$tools_module" "$target"
done

require_prerequisite "$tools_module" tools-disk tools-disk-rv
require_prerequisite "$tools_module" tools-disk tools-disk-la

for target in all env user fs-img kernel run runsimple comp comp-gdb ktest-run regression-run; do
    require_target os/make/rv64.mk "$target"
    require_target os/make/la64.mk "$target"
done

if awk '
    /^arch-build[[:space:]]*:/ && /(^|[[:space:]])prepare-cargo-config([[:space:]]|$)/ { found = 1 }
    END { exit(found ? 0 : 1) }
' "$repo_root/os/Makefile" 2>/dev/null; then
    pass 'os/Makefile arch-build requires prepare-cargo-config'
else
    fail 'os/Makefile arch-build must require prepare-cargo-config'
fi

if grep -Eq 'ARCH|arch' "$repo_root/os/Makefile" 2>/dev/null \
    && grep -Eq 'PROFILE|profile' "$repo_root/os/Makefile" 2>/dev/null \
    && grep -Eq 'filter|case|error|invalid|validate' "$repo_root/os/Makefile" 2>/dev/null; then
    pass 'os/Makefile validates ARCH and PROFILE for arch-build'
else
    fail 'os/Makefile arch-build must validate ARCH and PROFILE'
fi

if grep -Eq '\$\(MAKE\).*\$\(ARCH\)|\$\(ARCH\).*\$\(MAKE\)' \
    "$repo_root/os/Makefile" 2>/dev/null; then
    pass 'os/Makefile arch-build delegates through ARCH'
else
    fail 'os/Makefile arch-build must delegate through ARCH'
fi

for facade in arch-build kernel user image; do
    require_target os/Makefile "$facade"
    require_phony_target os/Makefile "$facade"
done

for facade in arch-build kernel; do
    if target_exists os/Makefile "$facade"; then
        require_body_match os/Makefile "$facade" \
            'validates ARCH exactly as rv64 or la64' \
            'filter[[:space:]]+rv64[[:space:]]+la64,[[:space:]]*\$\(ARCH\)'
        require_body_match os/Makefile "$facade" \
            'validates PROFILE exactly as normal or regression' \
            'filter[[:space:]]+normal[[:space:]]+regression,[[:space:]]*\$\(PROFILE\)'
        require_body_match os/Makefile "$facade" \
            'delegates to legacy build only' \
            '\$\(MAKE\).*build([[:space:]]|$)'
        require_body_match os/Makefile "$facade" \
            'forwards INITRAMFS_PROFILE' \
            'INITRAMFS_PROFILE[[:space:]]*='
        require_body_absent os/Makefile "$facade" \
            'must not delegate to run/test/clean facades' \
            '(^|[^[:alnum:]_-])(comp|run|regression-run|clean)([^[:alnum:]_-]|$)'
    fi
done

for facade in user image; do
    if target_exists os/Makefile "$facade"; then
        require_body_match os/Makefile "$facade" \
            'validates ARCH exactly as rv64 or la64' \
            'filter[[:space:]]+rv64[[:space:]]+la64,[[:space:]]*\$\(ARCH\)'
        require_body_match os/Makefile "$facade" \
            'accepts only PROFILE=normal' \
            'filter[[:space:]]+normal,[[:space:]]*\$\(PROFILE\)'
        require_body_absent os/Makefile "$facade" \
            'rejects PROFILE=regression' \
            'regression'
        if [ "$facade" = user ]; then
            legacy=user
        else
            legacy=fs-img
        fi
        require_body_match os/Makefile "$facade" \
            "delegates to legacy $legacy only" \
            "\\$\\(MAKE\\).*${legacy}([[:space:]]|$)"
        require_body_absent os/Makefile "$facade" \
            'must not delegate to build/kernel/run/test/clean facades' \
            '(^|[^[:alnum:]_-])(build|kernel|comp|run|regression-run|clean)([^[:alnum:]_-]|$)'
    fi
done

for facade in arch-build kernel user image; do
    if target_exists os/Makefile "$facade"; then
        require_body_absent os/Makefile "$facade" \
            'must not delegate through the root Makefile or CI' \
            '(\.\./Makefile|\.github|ci[-_])'
    fi
done

require_valid_facade_probe arch-build rv64 normal build
require_valid_facade_probe arch-build la64 regression build
require_valid_facade_probe kernel rv64 regression build
require_valid_facade_probe kernel la64 normal build
require_valid_facade_probe user rv64 normal user
require_valid_facade_probe user la64 normal user
require_valid_facade_probe image rv64 normal fs-img
require_valid_facade_probe image la64 normal fs-img

for facade in arch-build kernel user image; do
    case "$facade" in
        arch-build|kernel) legacy=build ;;
        user) legacy=user ;;
        image) legacy=fs-img ;;
    esac
    require_invalid_facade_probe "$facade" '' normal "$legacy"
    require_invalid_facade_probe "$facade" arm64 normal "$legacy"
    require_invalid_facade_probe "$facade" 'rv64 la64' normal "$legacy"
done

for facade in arch-build kernel; do
    require_invalid_facade_probe "$facade" rv64 '' build
    require_invalid_facade_probe "$facade" rv64 staging build
    require_invalid_facade_probe "$facade" rv64 'normal regression' build
done

for facade in user image; do
    if [ "$facade" = user ]; then
        legacy=user
    else
        legacy=fs-img
    fi
    require_invalid_facade_probe "$facade" rv64 '' "$legacy"
    require_invalid_facade_probe "$facade" rv64 regression "$legacy"
    require_invalid_facade_probe "$facade" rv64 staging "$legacy"
    require_invalid_facade_probe "$facade" rv64 'normal regression' "$legacy"
done

exit "$overall"
