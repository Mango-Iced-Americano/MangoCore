#!/bin/sh
# Validate command construction and failure gates without launching QEMU.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
failures=0

fail() { printf '%s\n' "FAIL: $*" >&2; failures=1; }
pass() { printf '%s\n' "PASS: $*"; }

dry_run=$(mktemp "${TMPDIR:-/tmp}/qemu-command-matrix.XXXXXX")
trap 'rm -f "$dry_run"' EXIT HUP INT TERM

python3 "$root/scripts/run_full_test.py" --dry-run --serial >"$dry_run"
for profile in normal competition derived-competition development debug regression ktest; do
    for arch in rv64 la64; do
        grep -F "$profile $arch:" "$dry_run" >/dev/null || fail "missing $profile $arch dry-run command"
    done
done

for arch in rv64 la64; do
    normal=$(grep -F "normal $arch:" "$dry_run")
    regression=$(grep -F "regression $arch:" "$dry_run")
    printf '%s\n' "$normal" | grep -o -- '-drive ' | wc -l | grep -qx '2' || fail "$arch normal must have exactly x0+x1"
    if printf '%s\n' "$regression" | grep -F -- '-drive ' >/dev/null; then
        fail "$arch regression must have zero drives"
    fi
    pass "$arch drive matrix"
done

for fixture in build-failure extraction-failure qemu-timeout qemu-nonzero missing-terminal-marker judge-missing-group judge-nonzero; do
    if python3 "$root/scripts/run_full_test.py" --fixture "$fixture"; then
        fail "$fixture unexpectedly succeeded"
    fi
    pass "$fixture fails closed"
done

for arch in rv64 la64; do
    make -C "$root/os" -n -f "make/$arch.mk" comp derived-comp regression-run ktest-run >/dev/null || fail "$arch Make dry-run failed"
done

exit "$failures"
