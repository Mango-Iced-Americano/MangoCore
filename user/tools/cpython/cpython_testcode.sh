#!/bin/sh
set -u

echo "#### OS COMP TEST GROUP START cpython-isolated ####"
CPYTHON_TEST_ROOT=${CPYTHON_TEST_ROOT:-/tools/tests/cpython}
export CPYTHON_TEST_ROOT
cd "$CPYTHON_TEST_ROOT" || {
    echo "[CPYTHON L0 FAIL] missing $CPYTHON_TEST_ROOT"
    echo "#### OS COMP TEST GROUP END cpython-isolated ####"
    exit 1
}

. ./run_cpython.sh

# Keep the read-only tools partition immutable on the real board.  An explicit
# directory is fail-closed so performance runs can require P4 ext4 rather than
# silently falling back to another filesystem.
configured_test_tmpdir=${CPYTHON_TEST_TMPDIR:-}
if [ -n "$configured_test_tmpdir" ]; then
    if /bin/busybox mkdir -p "$configured_test_tmpdir"; then
        CPYTHON_TEST_TMPDIR=$configured_test_tmpdir
    else
        echo "[CPYTHON ENV FAIL] cannot create configured tmpdir=$configured_test_tmpdir"
        exit 2
    fi
elif [ -d /scratch ] && /bin/busybox mkdir -p /scratch/cpython; then
    CPYTHON_TEST_TMPDIR=/scratch/cpython
    # The staged 2K1000LA target enables GMAC/DHCP and must not turn a broken
    # external network path into a successful group via L9 SKIP records.
    CPYTHON_L9_REQUIRE_NET=1
    export CPYTHON_L9_REQUIRE_NET
elif /bin/busybox touch "$CPYTHON_TEST_ROOT/.write-probe" 2>/dev/null; then
    /bin/busybox rm -f "$CPYTHON_TEST_ROOT/.write-probe"
    CPYTHON_TEST_TMPDIR=$CPYTHON_TEST_ROOT
else
    /bin/busybox mkdir -p /tmp/cpython
    CPYTHON_TEST_TMPDIR=/tmp/cpython
fi
export CPYTHON_TEST_TMPDIR
export TMPDIR="$CPYTHON_TEST_TMPDIR"
echo "[CPYTHON ENV] tmpdir=$CPYTHON_TEST_TMPDIR"

fail=0

run_sh() {
    name="$1" layer="$2"
    echo "[CPYTHON $layer START] $name"
    if "$CPYTHON_TEST_ROOT/$name"; then
        rc=0
    else
        rc=$?
    fi
    if [ "$rc" -eq 0 ]; then
        echo "[CPYTHON $layer PASS] $name"
    else
        echo "[CPYTHON $layer FAIL] $name exit=$rc"
        fail=1
    fi
}

run_py() {
    name="$1" layer="$2"
    echo "[CPYTHON $layer START] $name"
    if "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" "$CPYTHON_TEST_ROOT/$name"; then
        rc=0
    else
        rc=$?
    fi
    if [ "$rc" -eq 0 ]; then
        echo "[CPYTHON $layer PASS] $name"
    else
        echo "[CPYTHON $layer FAIL] $name exit=$rc"
        fail=1
    fi
}

# L3: Binary integrity
run_sh L3_check_files.sh L3

# L4: Minimal startup
run_sh L4_startup.sh L4

# L5: Core language features
run_py L5_language.py L5

# L6: Standard library modules
run_py L6_stdlib.py L6

# L7: Filesystem semantics
run_py L7_filesystem.py L7

# L8: Threading
run_py L8_thread.py L8

# L8: Subprocess
run_py L8_subprocess.py L8-SUBPROC

# L9: Networking (DNS/TCP/HTTPS may SKIP without network)
run_py L9_socket.py L9

# Performance sampling is deliberately separate from the 72-item functional
# gate.  Set this only in an explicitly budgeted QEMU/board run.
if [ "${CPYTHON_RUN_BENCHMARKS:-0}" = "1" ]; then
    echo "[CPYTHON BENCH SUITE START]"
    if "$CPYTHON_TEST_ROOT/cpython_benchmark.sh"; then
        echo "[CPYTHON BENCH SUITE PASS]"
    else
        rc=$?
        echo "[CPYTHON BENCH SUITE FAIL] exit=$rc"
        fail=1
    fi
fi

echo "#### OS COMP TEST GROUP END cpython-isolated ####"
exit "$fail"
