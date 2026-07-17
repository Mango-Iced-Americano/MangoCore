#!/bin/bash
# diag_smoke_test.sh — verify unified kernel diagnostics in QEMU
# Usage: Place on disk image or run via QEMU monitor
set -e

echo "=== DIAG SMOKE TEST ==="

# 1. Check interface exists
echo "[1/7] Checking /sys/kernel/ exists..."
ls /sys/kernel/ || { echo "FAIL: /sys/kernel/ not found"; exit 1; }

echo "[2/7] Checking stats files..."
ls /sys/kernel/stats/ || { echo "FAIL: /sys/kernel/stats/ not found"; exit 1; }
cat /sys/kernel/stats/stats_on  # should be 0
cat /sys/kernel/stats/profile   # should be core 1
cat /sys/kernel/stats/boot      # one-shot boot milestones must be readable
cat /sys/kernel/stats/anon_unmap # bounded anonymous-release window must exist

echo "[3/7] Enable core profile and run workload..."
echo core > /sys/kernel/stats/profile
echo 1 > /sys/kernel/stats/stats_on
echo 1 > /sys/kernel/stats/reset
# Generate some syscall activity
busybox seq 1 1000 > /dev/null
busybox ls -la / > /dev/null
busybox cat /proc/self/status > /dev/null

echo "[4/7] Read stats..."
echo "--- taskq ---"
cat /sys/kernel/stats/taskq
echo "--- timer ---"
cat /sys/kernel/stats/timer
echo "--- syscall ---"
cat /sys/kernel/stats/syscall

echo "[5/7] Verify counters are non-zero..."
SYS_TOTAL=$(cat /sys/kernel/stats/syscall | grep syscall_total | cut -d= -f2)
if [ "$SYS_TOTAL" -gt 0 ]; then
    echo "PASS: syscall_total=$SYS_TOTAL (non-zero, stats working!)"
else
    echo "WARN: syscall_total=$SYS_TOTAL (zero — check if perf_stats feature is on)"
fi

echo "[6/7] Verify stats_on=0 freezes counters..."
echo 0 > /sys/kernel/stats/stats_on
SYS_FROZEN_BEFORE=$(grep '^syscall_total=' /sys/kernel/stats/syscall | cut -d= -f2)
busybox seq 1 1000 > /dev/null
busybox ls -la / > /dev/null
SYS_FROZEN_AFTER=$(grep '^syscall_total=' /sys/kernel/stats/syscall | cut -d= -f2)
if [ "$SYS_FROZEN_BEFORE" != "$SYS_FROZEN_AFTER" ]; then
    echo "FAIL: counters changed while stats_on=0: $SYS_FROZEN_BEFORE -> $SYS_FROZEN_AFTER"
    exit 1
fi
echo "PASS: counters frozen at syscall_total=$SYS_FROZEN_AFTER"

echo "[7/7] Verify bounded profile selection and runtime counters..."
echo network_runtime > /sys/kernel/stats/profile
echo 1 > /sys/kernel/stats/reset
echo 1 > /sys/kernel/stats/stats_on
busybox true
busybox cat /proc/self/status > /dev/null
cat /sys/kernel/stats/net
RUNTIME_EXEC=$(grep '^runtime_exec_calls=' /sys/kernel/stats/net | cut -d= -f2)
if [ "$RUNTIME_EXEC" -le 0 ]; then
    echo "FAIL: runtime_exec_calls=$RUNTIME_EXEC"
    exit 1
fi
echo 0 > /sys/kernel/stats/stats_on

echo "=== DIAG SMOKE TEST DONE ==="
