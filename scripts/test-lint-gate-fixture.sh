#!/bin/sh
# test-lint-gate-fixture.sh — Verify that a newly introduced first-party
# unused warning causes `make lint` to fail.
#
# Usage:
#   sh scripts/test-lint-gate-fixture.sh --inject unused-first-party
#
# The fixture:
#   1. Preserves the committed lint baseline.
#   2. Injects an unused function into a first-party kernel source file
#      (`os/src/lint_fixture.rs`) that is
#      compiled as part of the kernel but never called.
#   3. Runs `make lint ARCH=rv64 MODE=debug` (the fastest single cell).
#   4. Verifies the lint gate exits nonzero.
#   5. Cleans up the injected file without changing the baseline.

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

# ---- 2. Inject a dead-code warning into first-party source ----
# First, capture the current state of main.rs for restoration
main_rs="$repo_root/os/src/main.rs"
main_backup=$(mktemp)
cp "$main_rs" "$main_backup"
trap 'cp "$main_backup" "$main_rs"; rm -f "$main_backup"; rm -f "$fixture_file" "$lint_output"' EXIT HUP INT TERM

# Create a fixture module file
fixture_file="$repo_root/os/src/lint_fixture.rs"
cat > "$fixture_file" << 'FIEOF'
// Lint fixture: intentionally unused function to test the lint gate.
// This file is removed by test-lint-gate-fixture.sh --inject.
fn intentionally_unused_lint_fixture_function() {}
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

# Remove cached target for this lint-check to force re-check
rm -rf "$repo_root/build/lint-check"

# ---- 3. Run make lint ----
printf 'Running make lint ARCH=rv64 MODE=debug (expecting FAILURE)...\n' >&2
lint_output=$(mktemp)
set +e
make lint ARCH=rv64 MODE=debug >"$lint_output" 2>&1
lint_exit=$?
set -e
cat "$lint_output"

# ---- 4. Verify it failed ----
if [ "$lint_exit" -eq 0 ]; then
    fail "lint gate returned 0 despite injected first-party warning"
elif grep -q 'new first-party warning: .* in src/lint_fixture.rs' "$lint_output"; then
    printf 'PASS: lint gate correctly rejected first-party warning (exit %d)\n' "$lint_exit"
else
    fail "lint gate failed for a reason other than the injected first-party warning"
fi

# ---- 5. Final summary ----
printf '\nPASS: test-lint-gate-fixture.sh --inject unused-first-party confirmed lint gate rejects new first-party warnings\n'
exit 0
