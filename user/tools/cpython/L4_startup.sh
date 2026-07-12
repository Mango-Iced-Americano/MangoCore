#!/bin/sh
set -u

fail=0

run_py_test() {
    testname="$1"
    shift
    echo "[CPYTHON L4] test: $testname"
    "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" "$@"
    rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "[CPYTHON L4] test: $testname PASS"
    else
        echo "[CPYTHON L4] test: $testname FAIL (exit=$rc)"
        fail=1
    fi
}

# Source the environment
. /tools/tests/cpython/run_cpython.sh

# Test 1: Binary executes and prints version
echo "[CPYTHON L4] test: python3 --version"
"$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" --version
rc=$?
if [ "$rc" -eq 0 ]; then
    echo "[CPYTHON L4] test: python3 --version PASS"
else
    echo "[CPYTHON L4] test: python3 --version FAIL (exit=$rc)"
    fail=1
fi

# Test 2: Verify exit codes
echo "[CPYTHON L4] test: exit code 37"
if "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" -S -E -s -c 'import sys; sys.exit(37)'; then
    rc=0
else
    rc=$?
fi
if [ "$rc" -eq 37 ]; then
    echo "[CPYTHON L4] test: exit code 37 PASS"
else
    echo "[CPYTHON L4] test: exit code 37 FAIL (got exit=$rc, expected 37)"
    fail=1
fi

# Test 3: Stdlib import
run_py_test "import sys, encodings" -S -E -s -c 'import sys, encodings; print(sys.version)'

# Test 4: Path discovery
run_py_test "sys.prefix discovery" -S -E -s -c 'import sys; print(sys.prefix)'

if [ "$fail" -eq 0 ]; then
    echo "[CPYTHON L4] startup OK"
else
    echo "[CPYTHON L4] startup FAIL"
fi

exit "$fail"
