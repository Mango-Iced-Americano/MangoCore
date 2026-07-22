#!/bin/sh
set -eu

repo_root=${1:-$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)}
case "$repo_root" in
    /*) ;;
    *) repo_root=$(CDPATH= cd -- "$repo_root" && pwd) ;;
esac

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

check_normal_recipe() {
    makefile=$1
    target=$2
    stub=$3

    [ -r "$repo_root/$makefile" ] || fail "missing Makefile: $makefile"
    if awk -v target="$target" -v stub="$stub" '
        $0 == target ": user" {
            matched += 1
            in_recipe = 1
            next
        }
        in_recipe && /^\t/ {
            line = $0
            sub(/^\t/, "", line)
            if (line ~ "^@touch src/" stub "([[:space:]]|$)") {
                print NR ":" $0
                found = 1
            }
            next
        }
        in_recipe && !/^\t/ {
            in_recipe = 0
        }
        END {
            if (matched != 1) {
                exit 2
            }
            exit(found ? 1 : 0)
        }
    ' "$repo_root/$makefile"; then
        status=0
    else
        status=$?
    fi
    case "$status" in
        0) echo "PASS: $makefile normal recipe does not touch src/$stub" ;;
        1) fail "$makefile normal recipe touches tracked stub src/$stub" ;;
        2) fail "$makefile must contain exactly one normal recipe for $target" ;;
        *) fail "unable to inspect normal recipe in $makefile" ;;
    esac
}

check_normal_recipe os/make/rv64.mk '$(INITRAMFS_CPIO_RV)' initramfs-rv.S
check_normal_recipe os/make/la64.mk '$(INITRAMFS_CPIO_LA)' initramfs-la.S

build_source=$repo_root/os/build.rs
main_source=$repo_root/os/src/main.rs
[ -r "$build_source" ] || fail "missing build script: $build_source"
[ -r "$main_source" ] || fail "missing kernel root source: $main_source"

grep -F -- 'MANGO_INITRAMFS_CPIO' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must consume the declared initramfs CPIO artifact path'
grep -F -- 'MANGO_USER_OUTPUT_ROOT' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must consume the declared user output root'
grep -F -- 'cargo:rerun-if-env-changed=MANGO_INITRAMFS_CPIO' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must track initramfs CPIO selection changes'
grep -F -- 'cargo:rerun-if-changed=' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must track generated initramfs inputs'
grep -F -- 'initramfs.S' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must generate the initramfs assembly in OUT_DIR'
grep -F -- 'preload_app.S' "$build_source" >/dev/null 2>&1 ||
    fail 'build.rs must generate preload assembly in OUT_DIR'
echo 'PASS: build.rs declares isolated initramfs artifact inputs and OUT_DIR output'

grep -F -- 'include_str!(concat!(env!("OUT_DIR"), "/initramfs.S"))' "$main_source" >/dev/null 2>&1 ||
    fail 'main.rs must include the build-generated initramfs assembly'
if grep -E 'include_str!\("initramfs(-regression)?-(rv|la)\.S"\)' "$main_source" >/dev/null 2>&1; then
    fail 'main.rs must not include tracked initramfs assembly stubs'
fi
if grep -E 'include_str!\("preload_app(-rv)?\.S"\)' "$main_source" >/dev/null 2>&1; then
    fail 'main.rs must not include tracked preload assembly stubs'
fi
echo 'PASS: main.rs includes only generated initramfs assembly'

if find "$repo_root/os/src" -type f -name '*.S' -exec \
    grep -El '\.incbin[[:space:]].*\.\./user/target' {} + | grep -q .; then
    fail 'tracked assembly must not reference ../user/target'
fi
echo 'PASS: tracked assembly contains no fixed user target incbin'

for makefile in os/make/rv64.mk os/make/la64.mk; do
    grep -F -- 'MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_' "$repo_root/$makefile" >/dev/null 2>&1 ||
        fail "$makefile must pass the selected absolute initramfs CPIO path to Cargo"
    grep -F -- 'MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))"' "$repo_root/$makefile" >/dev/null 2>&1 ||
        fail "$makefile must pass the selected absolute user output root to Cargo"
done
echo 'PASS: architecture recipes pass profile-aware isolated initramfs inputs'

for arch in rv64 la64; do
    target="$arch-kernel-build-only"
    if trace=$(make -C "$repo_root/os" -n "PROFILE=regression" "$target" 2>&1); then
        case "$trace" in
            *"INITRAMFS_PROFILE=regression"*"initramfs-regression-"*)
                echo "PASS: $target forwards the regression initramfs profile" ;;
            *) fail "$target must select the regression initramfs artifact" ;;
        esac
    else
        fail "$target regression dry-run must succeed"
    fi
done
