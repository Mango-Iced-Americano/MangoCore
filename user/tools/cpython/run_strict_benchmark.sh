#!/bin/sh
# Board-local launcher for the strict-aligned A/B matrix.  Keeping the full
# environment here avoids overflowing MangoCore's serial command-line limit.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <bm_name> <run_tag>" >&2
    exit 2
fi

benchmark=$1
run_tag=$2
case "$benchmark" in
    bm_bytesio|bm_chaos|bm_decimal|bm_dict|bm_fileio|bm_float|bm_fork|bm_hash|\
    bm_json_loads|bm_list|bm_nbody|bm_pidigits|bm_regex|bm_richards|bm_sort|\
    bm_spectral_norm|bm_string|bm_thread) ;;
    *) echo "unknown benchmark: $benchmark" >&2; exit 2 ;;
esac
case "$run_tag" in
    ""|*[!A-Za-z0-9._-]*) echo "invalid run tag: $run_tag" >&2; exit 2 ;;
esac

runtime_root=$(/bin/busybox readlink -f "$(/bin/busybox dirname "$0")")
suite_root=${CPYTHON_BENCH_SUITE:-/persist/pyperf/s}
result_root=/persist/pyperf/o/$run_tag
result_file=$result_root/$benchmark.jsonl
work_root=/persist/pyperf/w-$run_tag

/bin/busybox grep -Eq \
    '^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' \
    /proc/mounts
test ! -L /persist
test "$(blockdev --getsize64 /dev/sda4)" = 4294967296
case "$runtime_root:$suite_root" in
    /persist/*:/persist/*) ;;
    *) echo "runtime and suite must both be on P4 /persist" >&2; exit 3 ;;
esac
test "$(/bin/busybox readlink -f "$runtime_root")" = "$runtime_root"
test "$(/bin/busybox readlink -f "$suite_root")" = "$suite_root"

test -d "$suite_root/bench"
test ! -e "$result_file"
/bin/busybox mkdir -p "$result_root" "$work_root"
test "$(/bin/busybox readlink -f "$result_root")" = "$result_root"
test "$(/bin/busybox readlink -f "$work_root")" = "$work_root"

runtime_tag=$(/bin/busybox basename "$runtime_root")
case "$runtime_tag" in
    ????????????) ;;
    *) echo "unexpected strict runtime identity: $runtime_tag" >&2; exit 3 ;;
esac
set -- $(/bin/busybox sha256sum "$runtime_root/strict-runtime-manifest.json")
manifest_sha=$1

export CPYTHON_ROOT="$runtime_root"
export CPYTHON_BENCH_ROOT="$suite_root"
export CPYTHON_BENCH_WORK_BASE="$work_root"
export CPYTHON_BENCH_JSONL="$result_file"
export CPYTHON_BENCH_WARMUPS=1
export CPYTHON_BENCH_RUNS=1
export CPYTHON_BENCH_TIMEOUT=1800
export CPYTHON_BENCH_TARGET_STATS=1
export CPYTHON_STRICT_RUN_TAG="$run_tag"
export CPYTHON_RUNTIME_ARTIFACT_SHA12="$runtime_tag"
export CPYTHON_RUNTIME_MANIFEST_SHA256="$manifest_sha"

exec /bin/sh "$suite_root/cpython_benchmark.sh" "$benchmark"
