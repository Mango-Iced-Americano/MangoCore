#!/bin/sh
# Given the normal userspace lifecycle binaries, when T7 source is inspected,
# then PID1 policy, runner policy, and legacy entrypoints remain separated.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

require() {
    pattern=$1
    file=$2
    grep -Fq -- "$pattern" "$file" || fail "missing '$pattern' in $file"
}

for file in user/src/bin/initd.rs user/src/bin/test_runner.rs user/src/bin/init.rs user/src/bin/initproc.rs; do
    [ -f "$file" ] || fail "missing lifecycle source $file"
done

require 'PID1' user/src/bin/initd.rs
require 'getpid() != PID1' user/src/bin/initd.rs
require 'SIGCHLD' user/src/bin/initd.rs
require 'reap_orphans' user/src/bin/initd.rs
require 'MANGO_RUNNER_FAILURE' user/src/bin/initd.rs
require 'shutdown()' user/src/bin/initd.rs

require 'getpid() == 1 || getppid() != 1' user/src/bin/test_runner.rs
require 'return 1;' user/src/bin/test_runner.rs
require 'TEST_GROUPS' user/src/bin/test_runner.rs
require 'DEFAULT_LTP_EXCLUDE' user/src/bin/test_runner.rs

for shim in user/src/bin/init.rs user/src/bin/initproc.rs; do
    require '/sbin/init\0' "$shim"
    require 'exec(init' "$shim"
    lines=$(wc -l < "$shim")
    [ "$lines" -le 40 ] || fail "$shim is not a thin compatibility shim ($lines lines)"
    if grep -Eq 'TEST_GROUPS|DEFAULT_LTP_EXCLUDE|run_selected_groups|prepare_symlink' "$shim"; then
        fail "$shim contains runner policy: $shim"
    fi
done

require 'INITD_SRC="$INIT_DIR/initd"' os/build_initramfs.sh
require 'INITPROC_SRC="$INIT_DIR/initproc"' os/build_initramfs.sh
require 'RUNNER_SRC="$INIT_DIR/test_runner"' os/build_initramfs.sh
require '"$STAGE/sbin/init"' os/build_initramfs.sh
require '"$STAGE/initproc"' os/build_initramfs.sh
require '"$STAGE/usr/libexec/mangocore/test-runner"' os/build_initramfs.sh

printf 'PASS: init lifecycle contract\n'
