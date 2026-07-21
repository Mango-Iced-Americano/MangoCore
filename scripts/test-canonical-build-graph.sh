#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
mode=${1:-}
case "$mode" in
    --matrix)
        [ "${2:-}" = serial ] || { echo 'FAIL: --matrix requires serial' >&2; exit 2; }
        ;;
    --fixture)
        [ "${2:-}" = second-stage-failure ] || { echo 'FAIL: unknown fixture' >&2; exit 2; }
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT HUP INT TERM
        if make -C "$repo_root/os" \
            BUILD_ROOT="$tmp/build" \
            COMPAT_OUTPUT_DIR="$tmp/published" \
            CANONICAL_BUILD_FIXTURE=second-stage-failure \
            all >"$tmp/output" 2>&1; then
            echo 'FAIL: second-stage failure fixture must return nonzero' >&2
            exit 1
        fi
        if [ -e "$tmp/published/kernel-rv" ] || [ -e "$tmp/published/kernel-la" ] \
            || [ -e "$tmp/published/disk.img" ] || [ -e "$tmp/published/disk-la.img" ]; then
            echo 'FAIL: second-stage failure fixture published compatibility artifacts' >&2
            exit 1
        fi
        if [ ! -e "$tmp/build/rv64/release/normal/fixture-rv64" ]; then
            echo 'FAIL: second-stage failure fixture did not stage RV64 before LA64 failed' >&2
            exit 1
        fi
        printf '%s\n' 'FAIL: second-stage failure fixture rejected mixed root publication'
        exit 1
        ;;
    *)
        echo 'FAIL: expected --matrix serial or --fixture second-stage-failure' >&2
        exit 2
        ;;
esac

overall=0
pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; overall=1; }

require_phony() {
    target=$1
    if awk -v target="$target" '
        /^[[:space:]]*\.PHONY[[:space:]]*:/ {
            sub(/^[^:]*:/, "")
            count = split($0, words, /[[:space:]]+/)
            for (item = 1; item <= count; item++) if (words[item] == target) found = 1
        }
        END { exit(found ? 0 : 1) }
    ' "$repo_root/Makefile"; then
        pass "root $target is a formal phony entrypoint"
    else
        fail "root $target must be a formal phony entrypoint"
    fi
}

for target in all build kernel user image run test check lint clean; do
    require_phony "$target"
done

for arch in rv64 la64; do
    for profile in normal regression; do
        for build_mode in debug release; do
            output_root="$repo_root/.contract-output/$arch/$build_mode/$profile"
            if trace=$(make -C "$repo_root" -n \
                "ARCH=$arch" "MODE=$build_mode" "PROFILE=$profile" \
                "BUILD_ROOT=$repo_root/.contract-output" kernel 2>&1); then
                case "$trace" in
                    *"CARGO_TARGET_DIR=\"$output_root/kernel\""*)
                        pass "$arch $build_mode $profile kernel has an isolated output root"
                        ;;
                    *) fail "$arch $build_mode $profile kernel must use $output_root/kernel" ;;
                esac
            else
                fail "$arch $build_mode $profile kernel dry-run must succeed"
            fi
        done
    done
done

if trace=$(make -C "$repo_root" -n all 2>&1); then
    rv_line=$(printf '%s\n' "$trace" | awk '/make rv64_all/ { print NR; exit }')
    la_line=$(printf '%s\n' "$trace" | awk '/make la64_all/ { print NR; exit }')
    publish_line=$(printf '%s\n' "$trace" | awk '/publish-compatibility/ { print NR; exit }')
    if [ -n "$rv_line" ] && [ -n "$la_line" ] && [ -n "$publish_line" ] \
        && [ "$rv_line" -lt "$la_line" ] && [ "$la_line" -lt "$publish_line" ]; then
        pass 'root all stages RV64, then LA64, then publishes compatibility artifacts'
    else
        fail 'root all must serialize RV64, LA64, then compatibility publication'
    fi
else
    fail 'root all dry-run must succeed'
fi

exit "$overall"
