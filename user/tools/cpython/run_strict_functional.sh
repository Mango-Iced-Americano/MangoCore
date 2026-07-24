#!/bin/sh
# Run the existing L3-L9 functional gate against the side-loaded runtime while
# keeping every writable test path on P4 ext4.
set -eu

run_tag=${1:-}
case "$run_tag" in
    ""|*[!A-Za-z0-9._-]*) echo "invalid run tag: $run_tag" >&2; exit 2 ;;
esac

runtime_root=$(/bin/busybox readlink -f "$(/bin/busybox dirname "$0")")
work_root=/persist/pyperf/f-$run_tag

/bin/busybox grep -Eq \
    '^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' \
    /proc/mounts
test ! -L /persist
test "$(blockdev --getsize64 /dev/sda4)" = 4294967296
case "$runtime_root" in /persist/*) ;; *) exit 3 ;; esac
test "$(/bin/busybox readlink -f "$runtime_root")" = "$runtime_root"
/bin/busybox mkdir -p "$work_root"
test "$(/bin/busybox readlink -f "$work_root")" = "$work_root"

export CPYTHON_ROOT="$runtime_root"
export CPYTHON_TEST_ROOT="$runtime_root"
export CPYTHON_TEST_TMPDIR="$work_root"
export CPYTHON_L9_REQUIRE_NET=1
exec /bin/sh "$runtime_root/cpython_testcode.sh"
