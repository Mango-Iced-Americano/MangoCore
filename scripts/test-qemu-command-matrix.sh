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
        grep "^$profile $arch:" "$dry_run" >/dev/null || fail "missing $profile $arch dry-run command"
    done
done

for arch in rv64 la64; do
    normal=$(grep "^normal $arch:" "$dry_run")
    competition=$(grep "^competition $arch:" "$dry_run")
    derived_competition=$(grep "^derived-competition $arch:" "$dry_run")
    regression=$(grep "^regression $arch:" "$dry_run")
    ktest=$(grep "^ktest $arch:" "$dry_run")
    case "$arch" in
        rv64) image_arch=rv ;;
        la64) image_arch=la ;;
    esac
    printf '%s\n' "$normal" | grep -o -- '-drive ' | wc -l | grep -qx '2' || fail "$arch normal must have exactly x0+x1"
    if printf '%s\n' "$regression" | grep -F -- '-drive ' >/dev/null; then
        fail "$arch regression must have zero drives"
    fi
    if printf '%s\n' "$ktest" | grep -F -- '-drive ' >/dev/null; then
        fail "$arch ktest must have zero drives"
    fi
    printf '%s\n' "$competition" | grep -F -- "sdcard-$image_arch.img" >/dev/null || fail "$arch competition must use official x0"
    if printf '%s\n' "$competition" | grep -F -- "sdcard-$image_arch-derived.img" >/dev/null; then
        fail "$arch competition must not use derived x0"
    fi
    printf '%s\n' "$derived_competition" | grep -F -- "sdcard-$image_arch-derived.img" >/dev/null || fail "$arch derived-competition must use derived x0"
    if printf '%s\n' "$derived_competition" | grep -F -- "sdcard-$image_arch.img" | grep -Fv -- "sdcard-$image_arch-derived.img" >/dev/null; then
        fail "$arch derived-competition must not use official x0"
    fi
    pass "$arch drive matrix"
done

if ! PYTHONPATH="$root/scripts" python3 -c '
from full_test.runner import _has_abnormal_signature
import sys
import tempfile
from pathlib import Path
with tempfile.TemporaryDirectory() as directory:
    output = Path(directory) / "qemu.log"
    output.write_text("Kernel panic\n", encoding="utf-8")
    sys.exit(not _has_abnormal_signature(output))
'; then
    fail "abnormal signature probe must reject a fatal marker"
fi

for fixture in build-failure extraction-failure qemu-timeout qemu-nonzero missing-terminal-marker judge-missing-group judge-nonzero abnormal-signature; do
    fixture_stderr=$(mktemp "${TMPDIR:-/tmp}/qemu-fixture.XXXXXX")
    if python3 "$root/scripts/run_full_test.py" --fixture "$fixture" 2>"$fixture_stderr"; then
        fail "$fixture unexpectedly succeeded"
    fi
    if grep -F -- 'invalid choice' "$fixture_stderr" >/dev/null; then
        fail "$fixture is not registered by the CLI"
    fi
    rm -f "$fixture_stderr"
    pass "$fixture fails closed"
done

if ! PYTHONPATH="$root/scripts" python3 -c '
import json
import tempfile
from pathlib import Path
from full_test.runner import run_fixture

expected_stages = {
    "abnormal-signature": "abnormal-signature",
    "build-failure": "build",
    "extraction-failure": "extraction",
    "qemu-timeout": "qemu-timeout",
    "qemu-nonzero": "qemu-nonzero",
    "missing-terminal-marker": "missing-terminal-marker",
    "judge-nonzero": "judge",
    "judge-missing-group": "judge-groups",
}
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    for fixture, stage in expected_stages.items():
        if run_fixture(root, fixture) != 1:
            raise SystemExit(f"{fixture} unexpectedly succeeded")
    archives = sorted((root / "testresult").glob("archive_*/rv64"))
    if len(archives) != len(expected_stages):
        raise SystemExit("fixture archives were not created")
    for archive in archives:
        for name in ("build.log", "qemu.log", "result.json"):
            if not (archive / name).is_file():
                raise SystemExit(f"{archive} omitted {name}")
        payload = json.loads((archive / "result.json").read_text(encoding="utf-8"))
        if payload["status"] != "failed" or not payload["stage"]:
            raise SystemExit(f"{archive} has an invalid result payload")
'; then
    fail "fixtures must archive build.log, qemu.log, and failed result.json"
fi

for arch in rv64 la64; do
    make -C "$root/os" -n -f "make/$arch.mk" comp derived-comp regression-run ktest-run >/dev/null || fail "$arch Make target dry-run failed"
    for profile in normal competition derived-competition development debug regression ktest; do
        make_command=$(make -C "$root/os" -s -f "make/$arch.mk" qemu-profile-dry-run QEMU_PROFILE="$profile") || fail "$arch $profile Make command render failed"
        python_command=$(grep "^$profile $arch:" "$dry_run" | sed "s/^$profile $arch: //")
        [ "$make_command" = "$python_command" ] || fail "$arch $profile Make/Python command mismatch"
    done
done

for deprecated in \
    "make test-docker-parallel" \
    "$root/run_test.sh" \
    "$root/scripts/run_test_docker_parallel.sh"; do
    deprecated_stderr=$(mktemp "${TMPDIR:-/tmp}/deprecated-entrypoint.XXXXXX")
    if sh -c "$deprecated" 2>"$deprecated_stderr"; then
        fail "$deprecated unexpectedly succeeded"
    fi
    grep -F -- 'scripts/run_full_test.py' "$deprecated_stderr" >/dev/null || fail "$deprecated omitted the serial migration"
    rm -f "$deprecated_stderr"
done

exit "$failures"
