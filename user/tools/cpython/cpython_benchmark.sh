#!/bin/sh
set -u

BENCHMARKS="
bm_bytesio
bm_chaos
bm_decimal
bm_dict
bm_fileio
bm_float
bm_fork
bm_hash
bm_json_loads
bm_list
bm_nbody
bm_pidigits
bm_regex
bm_richards
bm_sort
bm_spectral_norm
bm_string
bm_thread
"

if [ "${1:-}" = "--list" ]; then
    printf '%s\n' "$BENCHMARKS" | /bin/busybox sed '/^$/d'
    exit 0
fi

CPYTHON_BENCH_ROOT="${CPYTHON_BENCH_ROOT:-$(/bin/busybox dirname "$0")}"
cd "$CPYTHON_BENCH_ROOT" || {
    echo "[CPYTHON BENCH FAIL] missing benchmark suite $CPYTHON_BENCH_ROOT"
    exit 127
}
CPYTHON_BENCH_ROOT=$(pwd)
CPYTHON_ROOT="${CPYTHON_ROOT:-/persist/python-runtime/current}"
. "$CPYTHON_ROOT/run_cpython.sh"
# Older board runtime images compute CPYTHON_LD without exporting it.  The
# fork benchmark needs the loader path in child Python processes as well.
export CPYTHON_LD

if [ "$#" -eq 0 ]; then
    set -- $BENCHMARKS
fi

require_scratch="${CPYTHON_BENCH_REQUIRE_SCRATCH:-0}"
configured_work_base="${CPYTHON_BENCH_WORK_BASE:-}"
if [ -n "$configured_work_base" ]; then
    if /bin/busybox mkdir -p "$configured_work_base" 2>/dev/null; then
        work_base="$configured_work_base"
    else
        echo "[CPYTHON BENCH FAIL] cannot create configured work base $configured_work_base"
        exit 2
    fi
elif [ "${MANGO_PYTHON_POLICY:-}" = p4-strict-align-v1 ]; then
    work_base=/persist/python/bench
    /bin/busybox mkdir -p "$work_base" 2>/dev/null || {
        echo "[CPYTHON BENCH FAIL] P4 benchmark work base is unavailable"
        exit 2
    }
elif [ -d /scratch ] && /bin/busybox mkdir -p /scratch/cpython-bench 2>/dev/null; then
    work_base=/scratch/cpython-bench
elif [ "$require_scratch" = "1" ]; then
    echo "[CPYTHON BENCH FAIL] real-board run requires writable P2 /scratch"
    exit 2
elif [ -d /persist ] && /bin/busybox mkdir -p /persist/cpython-bench 2>/dev/null; then
    work_base=/persist/cpython-bench
else
    /bin/busybox mkdir -p /tmp/cpython-bench 2>/dev/null || true
    work_base=/tmp/cpython-bench
fi

work_root="$work_base/run-$$"
/bin/busybox rm -rf "$work_root"
/bin/busybox mkdir -p "$work_root/tmp" "$work_root/storage" || {
    echo "[CPYTHON BENCH FAIL] cannot create $work_root"
    exit 2
}
cleanup() {
    /bin/busybox rm -rf "$work_root"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

export TMPDIR="$work_root/tmp"
export CPYTHON_BENCH_STORAGE_DIR="$work_root/storage"
export CPYTHON_BENCH_WARMUPS="${CPYTHON_BENCH_WARMUPS:-1}"
export CPYTHON_BENCH_RUNS="${CPYTHON_BENCH_RUNS:-1}"
export CPYTHON_BENCH_CHILD_TIMEOUT="${CPYTHON_BENCH_CHILD_TIMEOUT:-30}"
export PYTHONDONTWRITEBYTECODE=1
module_timeout="${CPYTHON_BENCH_TIMEOUT:-900}"

has_timeout=0
if /bin/busybox --list 2>/dev/null | /bin/busybox grep -qx timeout; then
    has_timeout=1
fi

fail=0
for benchmark in "$@"; do
    echo "[CPYTHON BENCH START] $benchmark tmp=$work_root"
    if [ "$has_timeout" -eq 1 ]; then
        /bin/busybox timeout -s KILL "$module_timeout" \
            "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" \
            "$CPYTHON_BENCH_ROOT/bench/bench_runner.py" \
            --warmups "$CPYTHON_BENCH_WARMUPS" --runs "$CPYTHON_BENCH_RUNS" "$benchmark"
        rc=$?
    else
        "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" \
            "$CPYTHON_BENCH_ROOT/bench/bench_runner.py" \
            --warmups "$CPYTHON_BENCH_WARMUPS" --runs "$CPYTHON_BENCH_RUNS" "$benchmark"
        rc=$?
    fi
    if [ "$rc" -eq 0 ]; then
        echo "[CPYTHON BENCH END] $benchmark PASS rc=0"
    else
        echo "[CPYTHON BENCH END] $benchmark FAIL rc=$rc"
        fail=1
    fi
done

exit "$fail"
