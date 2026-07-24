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

for file in user/src/bin/initd.rs user/src/bin/test_runner.rs user/src/bin/init.rs; do
    [ -f "$file" ] || fail "missing lifecycle source $file"
done

require 'PID1' user/src/bin/initd.rs
require 'getpid() != PID1' user/src/bin/initd.rs
require 'SIGCHLD' user/src/bin/initd.rs
require 'reap_orphans' user/src/bin/initd.rs
require 'MANGO_RUNNER_FAILURE' user/src/bin/initd.rs
require 'shutdown()' user/src/bin/initd.rs
require '/tools/etc\0' user/src/bin/initd.rs
require 'tools_ok' user/src/bin/initd.rs
require 'MS_BIND' user/src/bin/initd.rs
require 'initramfs/common/etc' os/make/tools-disk.mk

require '#[path = "test_runner/mod.rs"] mod runner;' user/src/bin/test_runner.rs
require 'runner::main' user/src/bin/test_runner.rs

for file in \
    user/src/bin/test_runner/mod.rs \
    user/src/bin/test_runner/process.rs \
    user/src/bin/test_runner/shell.rs \
    user/src/bin/test_runner/config/mod.rs \
    user/src/bin/test_runner/config/parse.rs \
    user/src/bin/test_runner/bootstrap/mod.rs \
    user/src/bin/test_runner/bootstrap/early.rs \
    user/src/bin/test_runner/bootstrap/layout.rs \
    user/src/bin/test_runner/bootstrap/libraries.rs \
    user/src/bin/test_runner/bootstrap/packages.rs \
    user/src/bin/test_runner/groups/mod.rs \
    user/src/bin/test_runner/groups/catalog.rs \
    user/src/bin/test_runner/groups/execute.rs \
    user/src/bin/test_runner/ltp/mod.rs \
    user/src/bin/test_runner/ltp/inline.rs \
    user/src/bin/test_runner/ltp/suite.rs \
    user/src/bin/test_runner/ltp/policy/mod.rs \
    user/src/bin/test_runner/ltp/policy/defaults.rs \
    user/src/bin/test_runner/ltp/policy/prefixes.rs \
    user/src/bin/test_runner/ltp/policy/exact.rs \
    user/src/bin/test_runner/instrumentation/mod.rs \
    user/src/bin/test_runner/instrumentation/drift.rs \
    user/src/bin/test_runner/smoke/mod.rs \
    user/src/bin/test_runner/smoke/timerfd.rs \
    user/src/bin/test_runner/smoke/posix_time.rs; do
    [ -f "$file" ] || fail "missing runner module $file"
done

require 'getpid() == 1 || getppid() != 1' user/src/bin/test_runner/mod.rs
require 'TEST_GROUPS' user/src/bin/test_runner/groups/catalog.rs
require 'DEFAULT_LTP_EXCLUDE' user/src/bin/test_runner/ltp/policy/defaults.rs
require '#### OS COMP TEST GROUP START' user/src/bin/test_runner/groups/execute.rs
require '#### OS COMP TEST GROUP END' user/src/bin/test_runner/groups/execute.rs
require '[initproc] config source=' user/src/bin/test_runner/config/parse.rs
require '[test-runner] lifecycle violation' user/src/bin/test_runner/mod.rs
for dead in run_unix_standalone_tests run_ltp_network_tests run_ltp_signal_tests should_enter_debug_shell; do
    if grep -Fq -- "$dead" user/src/bin/test_runner.rs; then
        fail "dead runner function remains: $dead"
    fi
done

for shim in user/src/bin/init.rs; do
    require '/sbin/init\0' "$shim"
    require 'exec(init' "$shim"
    lines=$(wc -l < "$shim")
    [ "$lines" -le 40 ] || fail "$shim is not a thin compatibility shim ($lines lines)"
    if grep -Eq 'TEST_GROUPS|DEFAULT_LTP_EXCLUDE|run_selected_groups|prepare_symlink' "$shim"; then
        fail "$shim contains runner policy: $shim"
    fi
done

require 'INITD_SRC="$INIT_DIR/initd"' scripts/build_initramfs.sh
require 'RUNNER_SRC="$INIT_DIR/test_runner"' scripts/build_initramfs.sh
require '"$STAGE/sbin/init"' scripts/build_initramfs.sh
require '"$STAGE/usr/libexec/mangocore/test-runner"' scripts/build_initramfs.sh

printf 'PASS: init lifecycle contract\n'
