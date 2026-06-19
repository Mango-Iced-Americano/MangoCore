#!/bin/bash
# diag_smoke_test.sh — verify unified kernel diagnostics in QEMU
# Usage: Place on disk image or run via QEMU monitor
set -e

echo "=== DIAG SMOKE TEST ==="

# 1. Check interface exists
echo "[1/5] Checking /sys/kernel/ exists..."
ls /sys/kernel/ || { echo "FAIL: /sys/kernel/ not found"; exit 1; }

echo "[2/5] Checking stats files..."
ls /sys/kernel/stats/ || { echo "FAIL: /sys/kernel/stats/ not found"; exit 1; }
cat /sys/kernel/stats/stats_on  # should be 0

echo "[3/5] Enable stats and run workload..."
echo 1 > /sys/kernel/stats/stats_on
echo 1 > /sys/kernel/stats/reset
# Generate some syscall activity
busybox seq 1 1000 > /dev/null
busybox ls -la / > /dev/null
busybox cat /proc/self/status > /dev/null

echo "[4/5] Read stats..."
echo "--- taskq ---"
cat /sys/kernel/stats/taskq
echo "--- timer ---"
cat /sys/kernel/stats/timer
echo "--- syscall ---"
cat /sys/kernel/stats/syscall

echo "[5/5] Verify counters are non-zero..."
SYS_TOTAL=$(cat /sys/kernel/stats/syscall | grep syscall_total | cut -d= -f2)
if [ "$SYS_TOTAL" -gt 0 ]; then
    echo "PASS: syscall_total=$SYS_TOTAL (non-zero, stats working!)"
else
    echo "WARN: syscall_total=$SYS_TOTAL (zero — check if perf_stats feature is on)"
fi

echo "=== DIAG SMOKE TEST DONE ==="
