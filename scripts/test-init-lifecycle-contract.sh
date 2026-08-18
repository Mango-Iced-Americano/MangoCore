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

for file in user/src/bin/init.rs user/src/bin/test_runner.rs; do
    [ -f "$file" ] || fail "missing lifecycle source $file"
done

require 'PID1' user/src/bin/init.rs
require 'getpid() != PID1' user/src/bin/init.rs
require 'SIGCHLD' user/src/bin/init.rs
require 'reap_orphans' user/src/bin/init.rs
require 'MANGO_RUNNER_FAILURE' user/src/bin/init.rs
require 'shutdown()' user/src/bin/init.rs
require 'profile=mainline' user/src/bin/init.rs
require 'PERSISTENT_ROOT_INITPROC' user/src/bin/init.rs
require 'PERSISTENT_ROOT_SHELL' user/src/bin/init.rs
require 'PERSISTENT_ROOT_BUSYBOX' user/src/bin/init.rs
require 'mainline exec /bin/busybox sh -i' user/src/bin/init.rs
require '/tools/etc\0' user/src/bin/init/mounts.rs
require 'const X_OK: u32 = 1;' user/src/bin/init/mounts.rs
require '"/initproc"' user/src/bin/init/mounts.rs
require 'setup_persistent_mounts' user/src/bin/init/mounts.rs
require 'MS_BIND' user/src/bin/init/mounts.rs
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

require 'INITD_SRC="$INIT_DIR/init"' scripts/build_initramfs.sh
require 'RUNNER_SRC="$INIT_DIR/test_runner"' scripts/build_initramfs.sh
require '"$STAGE/sbin/init"' scripts/build_initramfs.sh
require '"$STAGE/test-runner"' scripts/build_initramfs.sh

# 2K1000 mainline must select a named persistent partition, keep the
# crash-consistent Rust backend, and expose a real device flush barrier.  This
# prevents a later merge from preserving the chroot code while silently
# reverting the storage path to initramfs or a volatile write cache.
require 'root=/dev/sda3' os/make/la64.mk
require 'EXT4_BACKEND ?= another' os/make/ext4_backend.mk
require 'fn supports_reliable_flush(&self) -> bool' os/src/drivers/block/sata_blk.rs
require 'self.0.lock().flush()' os/src/drivers/block/sata_blk.rs
require 'if !read_only && !block_device.supports_reliable_flush()' os/src/fs/ext4_another/fs.rs
require 'replace_symlink(staging / "sbin" / "init", "../bin/busybox")' scripts/make_2k1000_full_test_disk.py
require '"^has_journal"' scripts/make_2k1000_full_test_disk.py
require 'tools root must provide usr/bin/python3 launcher' scripts/make_2k1000_full_test_disk.py
require 'tools root must provide usr/bin/{command} launcher' scripts/make_2k1000_full_test_disk.py
require 'local_boot_kernel' scripts/make_2k1000_tools_partition.py
require '::sysinit:/etc/init.d/rcS' user/tools/loongarch64/etc/inittab
require '::askfirst:/bin/busybox sh -i' user/tools/loongarch64/etc/inittab
require 'CPYTHON_ROOT="${CPYTHON_ROOT:-/tests/cpython}"' user/tools/loongarch64/usr/bin/python3
require 'CURL_ROOT=/curl-runtime' user/tools/loongarch64/usr/bin/curl
require 'root=/persist/apk-root' user/tools/loongarch64/usr/bin/apk
require 'mango-apk-bootstrap' user/tools/loongarch64/usr/bin/persist-shell
require 'mount -t ext4 /dev/sda4 /persist' user/tools/loongarch64/etc/init.d/rcS
require 'P4 package state mounted at /persist' user/tools/loongarch64/etc/init.d/rcS
require 'LOCAL_KERNEL = "/boot/kernel-A.ui"' scripts/configure_2k1000_local_boot.py
require 'BOOTCMD = "run mango_local_boot; run mango_tftp_boot; run mango_bootcmd_legacy"' scripts/configure_2k1000_local_boot.py
require '"mw.l ${loadaddr} 0 1;"' scripts/configure_2k1000_local_boot.py
require 'console.command("setenv bootdelay 3")' scripts/configure_2k1000_local_boot.py
require '"--install-ssd-kernel"' scripts/boot_2k1000_tftp.py
require '"--monitor-only"' scripts/boot_2k1000_tftp.py
require 'no U-Boot command will be sent automatically' scripts/boot_2k1000_tftp.py
require 'console.monitor_existing_boot()' scripts/boot_2k1000_tftp.py
require '"--ssd-backup-id"' scripts/boot_2k1000_tftp.py
require '"--confirm-ssd-p3-start"' scripts/boot_2k1000_tftp.py
require '"--verify-kernel"' scripts/write_2k1000_p3.py
require 'P3 manifest kernel SHA-256 does not match --verify-kernel' scripts/write_2k1000_p3.py
require 'P3 must be mounted read-only at /tools, /sdcard, or /' scripts/board/backup_2k1000_p3.sh
require '440|660' scripts/board/backup_2k1000_p3.sh
require 'P3_BACKUP_SOURCE_READY' scripts/board/prepare_2k1000_p3_backup.sh
require 'p3_backup_prepare_readonly_source' scripts/backup_2k1000_p3.py
require 'ensure_interface("en8", args.host_ip, "255.255.255.0", True)' scripts/backup_2k1000_p3.py
require '(args.run_dir / "raw").mkdir(exist_ok=True)' scripts/backup_2k1000_p3.py
require 'persistent P3 image' scripts/backup_2k1000_p3.py
require 'wrapped.encode("utf-8") + b' scripts/backup_2k1000_p3.py
require 'P4 package state is mounted by P3 rcS' os/make/la64.mk

printf 'PASS: init lifecycle contract\n'
