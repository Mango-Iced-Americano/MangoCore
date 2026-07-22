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

clean_tmp=$(mktemp -d)
trap 'rm -rf "$clean_tmp"' EXIT HUP INT TERM
os_build_root="$clean_tmp/os-build"
root_build_root="$clean_tmp/root-build"
compat_output="$clean_tmp/compat"

mkdir -p "$os_build_root/rv64" "$os_build_root/la64" "$root_build_root/rv64" "$root_build_root/la64" "$compat_output"
touch "$os_build_root/rv64/sentinel" "$os_build_root/la64/sentinel"

if clean_trace=$(make -C "$repo_root/os" -n BUILD_ROOT="$os_build_root" clean 2>&1); then
    case "$clean_trace" in
        *'for arch in rv64 la64; do'*'/rv64/release/normal/kernel/kernel-rv'* ) pass 'os clean dispatches the RV64 architecture clean route' ;;
        * ) fail 'os clean must dispatch the RV64 architecture clean route' ;;
    esac
    case "$clean_trace" in
        *'for arch in rv64 la64; do'*'/la64/release/normal/kernel/kernel-la'* ) pass 'os clean dispatches the LA64 architecture clean route' ;;
        * ) fail 'os clean must dispatch the LA64 architecture clean route' ;;
    esac
else
    fail 'os clean dry-run must succeed'
fi

if make -C "$repo_root/os" BUILD_ROOT="$os_build_root" clean >/dev/null 2>&1 && [ ! -e "$os_build_root" ]; then
    pass 'os clean removes the supplied BUILD_ROOT after both architecture routes'
else
    fail 'os clean must remove the supplied BUILD_ROOT'
fi

for artifact in kernel-rv kernel-la disk.img disk-la.img; do
    touch "$compat_output/$artifact"
done
touch "$compat_output/unrelated-sentinel"

if make -C "$repo_root" BUILD_ROOT="$root_build_root" COMPAT_OUTPUT_DIR="$compat_output" clean >/dev/null 2>&1 \
    && [ ! -e "$root_build_root" ] \
    && [ ! -e "$compat_output/kernel-rv" ] \
    && [ ! -e "$compat_output/kernel-la" ] \
    && [ ! -e "$compat_output/disk.img" ] \
    && [ ! -e "$compat_output/disk-la.img" ] \
    && [ -e "$compat_output/unrelated-sentinel" ]; then
    pass 'root clean removes only known compatibility artifacts and the supplied BUILD_ROOT'
else
    fail 'root clean must remove known compatibility artifacts without touching unrelated files'
fi

exit "$overall"
