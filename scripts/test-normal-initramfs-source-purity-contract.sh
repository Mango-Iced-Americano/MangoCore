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
[ -r "$build_source" ] || fail "missing build script: $build_source"

for arch in rv la; do
    declaration="println!(\"cargo:rerun-if-changed=../fs-img-dir/initramfs-$arch.cpio\");"
    grep -F -- "$declaration" "$build_source" >/dev/null 2>&1 ||
        fail "missing normal $arch initramfs Cargo freshness declaration"
    echo "PASS: build.rs declares normal $arch initramfs Cargo freshness input"
done
