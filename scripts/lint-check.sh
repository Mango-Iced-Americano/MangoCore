#!/bin/sh
# lint-check.sh — Baseline-aware warning gate for MangoCore kernel.
#
# Collects (or verifies) compiler warnings for a given arch/mode,
# classifies by ownership (first-party / maintained / vendor), and
# compares against the committed baseline.
#
# Usage:
#   lint-check.sh --arch <rv64|la64> --mode <debug|release>        # verify
#   lint-check.sh --capture-baseline --arch <rv64|la64> --mode <debug|release>
#   lint-check.sh --help
#
# Exit status:
#   0 — warnings match baseline (no new first-party warnings)
#   1 — new first-party warning(s) found or stale baseline
#   2 — usage error

set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
baseline_dir="$repo_root/lint-baseline"

arch=
mode=
capture=0

usage() {
    sed -n 's/^# //p; /^$/q' "$0" >&2
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --arch) arch=$2; shift 2 ;;
        --mode) mode=$2; shift 2 ;;
        --capture-baseline) capture=1; shift ;;
        --help) usage ;;
        *) usage ;;
    esac
done

[ -n "$arch" ] || usage
[ -n "$mode" ] || usage
case "$arch" in rv64|la64) ;; *) usage ;; esac
case "$mode" in debug|release) ;; *) usage ;; esac

baseline_file="$baseline_dir/$arch-$mode.txt"

# ---- Helper functions ----
pass() { printf 'PASS: [%s/%s] %s\n' "$arch" "$mode" "$*"; }
fail() { printf 'FAIL: [%s/%s] %s\n' "$arch" "$mode" "$*" >&2; overall=1; }
warn() { printf 'WARN: [%s/%s] %s\n' "$arch" "$mode" "$*" >&2; }
overall=0

# ---- 1. Determine target / board / features ----
case "$arch" in
    rv64)
        target="riscv64gc-unknown-none-elf"
        board="rvqemu"
        # initramfs is the canonical boot root for lint builds.
        features="board_${board},block_virt,oom_handler,initramfs"
        ;;
    la64)
        target="loongarch64-unknown-linux-gnu"
        board="laqemu"
        features="board_${board},block_virt_pci,oom_handler,initramfs"
        ;;
esac

release_flag=""
[ "$mode" = "release" ] && release_flag="--release"

# ---- 2. Create minimal initramfs stub for build.rs ----
# build.rs requires MANGO_INITRAMFS_CPIO to point to an existing file
# when the initramfs feature is enabled.
initramfs_stub="$repo_root/build/lint-check/$arch-$mode/initramfs-stub.cpio"
if [ ! -f "$initramfs_stub" ]; then
    mkdir -p "$(dirname "$initramfs_stub")"
    # Minimal empty cpio archive (TRAILER record only)
    printf '%s' '07070100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000TRAILER!!!' > "$initramfs_stub"
fi

# build.rs requires MANGO_USER_OUTPUT_ROOT to exist as a directory
# with subdirectories for preload payloads (stub ELF binaries).
user_output_root="$repo_root/build/lint-check/$arch-$mode/user-output"

create_stub_elf() {
    # Minimal valid 64-bit ELF header (ET_EXEC, appropriate arch)
    printf '\177ELF\002\001\001\000\000\000\000\000\000\000\000\000'
    # e_type = ET_EXEC (2), e_machine = depend on target
    printf '\002\000'
    case "$arch" in
        rv64) printf '\363\000' ;;  # EM_RISCV = 243
        la64) printf '\102\002' ;;  # EM_LOONGARCH = 258
    esac
    printf '\001\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000'
    printf '\100\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000'
    printf '\000\000\000\000\100\000\070\000\001\000\000\000\000\000\000\000'
}

case "$arch" in
    rv64) target_arch_dir="riscv64gc-unknown-none-elf" ;;  # MATCH!
    la64) target_arch_dir="loongarch64-unknown-linux-gnu" ;;
esac

preload_dir="$user_output_root/$target_arch_dir/$mode"
mkdir -p "$preload_dir"

# Create stub binaries needed by build.rs generate_preload_assembly()
for f in initproc fs_test ltprunner; do
    [ -f "$preload_dir/$f" ] || create_stub_elf > "$preload_dir/$f"
done

[ -f "$repo_root/os_test.conf" ] || touch "$repo_root/os_test.conf"

# ---- 3. Run cargo check, capture stderr ----
build_root="$repo_root/build/lint-check/$arch-$mode/target"

# Use the os/ directory's Cargo.toml
cd "$repo_root/os"

warnings_raw=$(mktemp)
trap 'rm -f "$warnings_raw"' EXIT HUP INT TERM

CARGO_TARGET_DIR="$build_root" \
MANGO_CMDLINE="mango.mode=lint" \
MANGO_INITRAMFS_CPIO="$initramfs_stub" \
MANGO_USER_OUTPUT_ROOT="$user_output_root" \
MANGO_USER_OUTPUT_MODE="$mode" \
LOG=off \
    cargo check $release_flag \
        --target "$target" \
        --features "$features" \
        2>"$warnings_raw" || {
    fail "cargo check failed (exit $?)"
    cat "$warnings_raw" >&2
    exit "$overall"
}

# ---- 4. Parse warnings from stderr ----
normalized=$(mktemp)
trap 'rm -f "$warnings_raw" "$normalized"' EXIT HUP INT TERM

# Strip ANSI escapes and normalize line endings
sed -e 's/\x1b\[[0-9;]*m//g' -e 's/\r$//' "$warnings_raw" > "$normalized"

summary=$(mktemp)
trap 'rm -f "$warnings_raw" "$normalized" "$summary"' EXIT HUP INT TERM

# Parse each warning block into category:code:file tuples
awk '
function classify_file(path) {
    # First-party: kernel sources (the lint target is the os crate).
    if (path ~ /^os\/src\// || path ~ /^src\//) return "first-party"
    # Maintained dependency: local core library under active project ownership.
    if (path ~ /(^|\/)libs\/mango-kernel-core\/src\//) return "maintained"
    # Vendored/upstream dependencies are recorded but never gate first-party work.
    if (path ~ /(^|\/)dependency\// || path ~ /(^|\/)vendor\//) return "vendor"
    if (path ~ /\/target\//) return "generated"
    return "vendor"
}

BEGIN {
    in_warning = 0
    cur_code = ""
    cur_file = ""
}

/^[[:space:]]*warning: / {
    if (in_warning && cur_file != "") {
        cat = classify_file(cur_file)
        if (cat != "generated") {
            if (cur_code == "") cur_code = "unknown"
            printf "%s:%s:%s\n", cat, cur_code, cur_file
        }
    }
    in_warning = 1
    cur_code = ""
    cur_file = ""
    next
}

in_warning && /^[[:space:]]*--> / {
    loc = $0
    sub(/^[[:space:]]*--> /, "", loc)
    sub(/:.*$/, "", loc)
    # Normalize path relative to repo_root
    gsub(/.*\/os\//, "os/", loc)
    gsub(/.*\/user\//, "user/", loc)
    cur_file = loc
    next
}

in_warning && /^[[:space:]]*=[[:space:]]*note:/ {
    note = $0
    if (match(note, /#\[warn\(([^)]+)\)\]/, arr)) cur_code = arr[1]
    next
}

in_warning && /^[[:space:]]*=[[:space:]]*/ { next }
in_warning && /^[[:space:]]*[0-9]+[[:space:]]*\|/ { next }

in_warning && /^[[:space:]]*$/ {
    if (cur_file != "") {
        cat = classify_file(cur_file)
        if (cat != "generated") {
            if (cur_code == "") cur_code = "unknown"
            printf "%s:%s:%s\n", cat, cur_code, cur_file
        }
    }
    in_warning = 0
    cur_code = ""
    cur_file = ""
    next
}

in_warning { next }

END {
    if (in_warning && cur_file != "") {
        cat = classify_file(cur_file)
        if (cat != "generated") {
            if (cur_code == "") cur_code = "unknown"
            printf "%s:%s:%s\n", cat, cur_code, cur_file
        }
    }
}
' "$normalized" | sort -u > "$summary"

# ---- 5. Count by category ----
count_first=$(grep -c '^first-party:' "$summary" || true)
count_maintained=$(grep -c '^maintained:' "$summary" || true)
count_vendor=$(grep -c '^vendor:' "$summary" || true)

# ---- 6. Capture or compare ----
if [ "$capture" = "1" ]; then
    mkdir -p "$baseline_dir"
    {
        printf '# Lint baseline: %s %s\n' "$arch" "$mode"
        printf '# Generated by scripts/lint-check.sh --capture-baseline\n'
        printf '# Format: <category>:<lint_code>:<file>\n'
        printf '\n'
        cat "$summary"
        printf '\n'
        printf '# Summary\n'
        printf 'first-party: %d\n' "$count_first"
        printf 'maintained: %d\n' "$count_maintained"
        printf 'vendor: %d\n' "$count_vendor"
    } > "$baseline_file"
    pass "baseline captured: $count_first fp, $count_maintained mt, $count_vendor vd"
    exit "$overall"
fi

# ---- 7. Compare with baseline ----
if [ ! -f "$baseline_file" ]; then
    fail "no baseline at $baseline_file"
    exit "$overall"
fi

baseline_tuples=$(mktemp)
current_tuples=$(mktemp)
trap 'rm -f "$warnings_raw" "$normalized" "$summary" "$baseline_tuples" "$current_tuples"' EXIT HUP INT TERM

# IMPORTANT: exclude summary lines like "first-party: 175" (space after colon)
# by requiring a non-space character after the category prefix.
awk -F: '/^(first-party|maintained|vendor):[^ \t]/ && !/^#/ { print $1":"$2":"$3 }' "$baseline_file" | sort -u > "$baseline_tuples"
sort -u "$summary" > "$current_tuples"

new_warnings=$(comm -13 "$baseline_tuples" "$current_tuples" || true)
resolved_warnings=$(comm -23 "$baseline_tuples" "$current_tuples" || true)

new_fp=$(printf '%s\n' "$new_warnings" | grep '^first-party:' || true)
resolved_fp=$(printf '%s\n' "$resolved_warnings" | grep '^first-party:' || true)

if [ -n "$new_fp" ]; then
    # The reporting loop is a pipeline and therefore runs in a subshell in POSIX
    # sh. Set the gate result in the parent before emitting each diagnostic.
    overall=1
    printf '%s\n' "$new_fp" | while IFS=: read -r cat code file; do
        fail "new first-party warning: $code in $file"
    done
fi

if [ -n "$resolved_fp" ]; then
    printf '%s\n' "$resolved_fp" | head -5 | while IFS=: read -r cat code file; do
        warn "first-party warning resolved: $code in $file"
    done
    fail "stale baseline — run --capture-baseline"
fi

new_other=$(printf '%s\n' "$new_warnings" | grep -v '^first-party:' || true)
resolved_other=$(printf '%s\n' "$resolved_warnings" | grep -v '^first-party:' || true)

if [ -n "$new_other" ]; then
    printf '%s\n' "$new_other" | while IFS=: read -r cat code file; do
        warn "new $cat warning (ungated): $code in $file"
    done
fi
if [ -n "$resolved_other" ]; then
    printf '%s\n' "$resolved_other" | while IFS=: read -r cat code file; do
        warn "resolved $cat warning: $code in $file"
    done
fi

if [ "$overall" -eq 0 ]; then
    pass "first-party warnings match baseline ($count_first fp, $count_maintained mt, $count_vendor vd)"
fi
exit "$overall"
