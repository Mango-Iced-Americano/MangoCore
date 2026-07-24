#!/bin/sh

set -eu

usage() {
    echo "usage: $0 LOG_PATH QEMU_EXIT_STATUS" >&2
    exit 2
}

[ "$#" -eq 2 ] || usage
log_path=$1
qemu_status=$2

[ -r "$log_path" ] || {
    echo "error: log is not readable: $log_path" >&2
    exit 2
}

case $qemu_status in
    ''|*[!0-9]*)
        echo "error: QEMU exit status must be a non-negative integer: $qemu_status" >&2
        exit 2
        ;;
esac

has_marker() {
    marker_found=1
    for marker
    do
        if grep -F "$marker" "$log_path" >/dev/null 2>&1; then
            marker_found=0
            return 0
        fi
    done
    return 1
}

# The LA64 fixture markers are stable contract tokens. The source markers are
# accepted too, so this check can classify the current regression console log.
if has_marker \
    '[LA64 REGRESSION TERMINAL: FAIL]' \
    '[L4 REGRESSION RESULT: FAIL]' \
    '[L4 REGRESSION FAILED]'; then
    state=TEST_FAILURE
elif has_marker \
    '[LA64 REGRESSION TERMINAL: PASS]' \
    '[L4 REGRESSION RESULT: PASS]' \
    '[L4 REGRESSION PASSED]'; then
    if [ "$qemu_status" -eq 0 ]; then
        state=PASS
    else
        state=SHUTDOWN_FAILURE
    fi
elif ! has_marker \
    '[LA64 REGRESSION KERNEL]' \
    '[kernel] regression mode' \
    '[kernel] regression initramfs'; then
    state=ENTRY_FAILURE
elif ! has_marker \
    '[LA64 REGRESSION PID1]' \
    '[regression_init] starting regression suite'; then
    state=BLOCKED_STAGE1_PRE_ENTRY
else
    state=BLOCKED_STAGE1_POST_ENTRY
fi

printf 'STATE=%s STATUS=%s\n' "$state" "$qemu_status"
