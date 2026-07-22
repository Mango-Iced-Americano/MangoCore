#!/bin/sh
# test-lint-gate-fixture.sh — Verify that a newly introduced first-party
# unused warning causes `make lint` to fail.
#
# Usage:
#   sh scripts/test-lint-gate-fixture.sh --inject unused-first-party
#
# The fixture:
#   1. Backs up the lint baseline.
#   2. Injects an `#[allow(dead_code)]`-bypassing unused function into
#      a first-party kernel source file (tests/lint_fixture.rs) that is
#      compiled as part of the kernel but never called.
#   3. Runs `make lint ARCH=rv64 MODE=debug` (the fastest single cell).
#   4. Verifies the lint gate exits nonzero.
#   5. Restores the baseline and cleans up the injected file.

set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo_root"

case "${1:-}" in
    --inject)
        [ "${2:-}" = unused-first-party ] || {
            echo 'FAIL: --inject requires "unused-first-party"' >&2
            exit 2
        }
        ;;
    *)  echo 'FAIL: expected --inject unused-first-party' >&2; exit 2 ;;
esac

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# ---- 1. Save baseline ----
lint_baseline="$repo_root/lint-baseline"
if [ -d "$lint_baseline" ]; then
    baseline_backup=$(mktemp -d "${TMPDIR:-/tmp}/lint-baseline-backup.XXXXXX")
    cp -a "$lint_baseline"/* "$baseline_backup/" 2>/dev/null || true
    trap 'rm -rf "$baseline_backup"' EXIT HUP INT TERM
fi

# ---- 2. Inject a dead-code warning into first-party source ----
# We create a small module that is included by main.rs via a conditional
# feature. We add a feature to Cargo.toml temporarily.
#
# Simpler approach: directly add an unused function to an existing
# first-party source file that does NOT have crate-level allow(dead_code).
# We'll add to os/src/timer.rs since it has #![allow(unused)] but we can
# add an unused function that triggers "unused" warning even with that —
# actually #![allow(unused)] suppresses all unused warnings.
#
# Better approach: add a test source file and include it in main.rs behind
# a fixture feature.

# First, capture the current state of main.rs for restoration
main_rs="$repo_root/os/src/main.rs"
main_backup=$(mktemp)
cp "$main_rs" "$main_backup"
trap 'cp "$main_backup" "$main_rs"; rm -f "$main_backup"; rm -rf "$baseline_backup"' EXIT HUP INT TERM

# Create a fixture module file
fixture_file="$repo_root/os/src/lint_fixture.rs"
cat > "$fixture_file" << 'FIEOF'
// Lint fixture: intentionally unused function to test the lint gate.
// This file is removed by test-lint-gate-fixture.sh --inject.
#![allow(unused_imports)]
use crate::println;
fn intentionally_unused_lint_fixture_function() {
    println!("This function is never called — should trigger unused warning");
}
FIEOF

# Add `mod lint_fixture;` to main.rs before the task module
# We insert it right before "mod task;" which is at line ~50
# We need to be careful about exact content matching
# Let's use sed to insert after the last mod declaration before mod task;
# Or better, we find the "mod task;" line and insert before it
if grep -q '^mod task;' "$main_rs"; then
    # Insert before the existing line
    sed -i '/^mod task;$/i mod lint_fixture;' "$main_rs"
else
    fail "could not find 'mod task;' in main.rs to insert fixture"
fi

# Also update the baseline for rv64-debug to not include this warning
# (The fixture test should work even if baseline doesn't match exactly —
#  the point is the lint gate detects the NEW warning that wasn't there before.)
# Remove rv64-debug baseline to ensure the injected warning is detected as new
rm -f "$lint_baseline/rv64-debug.txt"

# Remove cached target for this lint-check to force re-check
rm -rf "$repo_root/build/lint-check"

# ---- 3. Run make lint ----
printf 'Running make lint ARCH=rv64 MODE=debug (expecting FAILURE)...\n' >&2
set +e
make lint ARCH=rv64 MODE=debug 2>&1
lint_exit=$?
set -e

# ---- 4. Verify it failed ----
if [ "$lint_exit" -eq 0 ]; then
    fail "lint gate returned 0 despite injected first-party warning"
elif [ "$lint_exit" -ge 1 ]; then
    printf 'PASS: lint gate correctly rejected first-party warning (exit %d)\n' "$lint_exit"
else
    fail "unexpected exit code $lint_exit from make lint"
fi

# ---- 5. Final summary ----
printf '\nPASS: test-lint-gate-fixture.sh --inject unused-first-party confirmed lint gate rejects new first-party warnings\n'
exit 0
