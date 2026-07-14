#!/bin/sh
set -eu

CPYTHON_ROOT="${CPYTHON_ROOT:-/tools/tests/cpython}"

if [ -x "$CPYTHON_ROOT/lib/ld-musl-riscv64.so.1" ]; then
    CPYTHON_LD="$CPYTHON_ROOT/lib/ld-musl-riscv64.so.1"
elif [ -x "$CPYTHON_ROOT/lib/ld-musl-loongarch64.so.1" ]; then
    CPYTHON_LD="$CPYTHON_ROOT/lib/ld-musl-loongarch64.so.1"
else
    echo "python3: CPython runtime is unavailable: missing musl loader under $CPYTHON_ROOT/lib" >&2
    exit 127
fi

CPYTHON_PY="$CPYTHON_ROOT/usr/bin/python3"
if [ ! -x "$CPYTHON_PY" ]; then
    echo "python3: CPython runtime is unavailable: missing $CPYTHON_PY" >&2
    exit 127
fi

if [ -d /scratch ] && /bin/busybox mkdir -p /scratch/python/tmp /scratch/python/user 2>/dev/null; then
    export TMPDIR=/scratch/python/tmp
    export PYTHONUSERBASE=/scratch/python/user
else
    /bin/busybox mkdir -p /tmp/python 2>/dev/null || true
    export TMPDIR=/tmp/python
fi

# The runtime lives on the read-only tools partition. Keep bytecode outside that
# tree so repeated imports do not reparse and recompile every stdlib source file.
# Prefer the ext4 persistent partition, then the validated scratch partition,
# and finally tmpfs. CPython's normal pyc validation handles runtime updates.
if [ -z "${PYTHONPYCACHEPREFIX:-}" ]; then
    if [ -d /persist ] && /bin/busybox mkdir -p /persist/python/pycache 2>/dev/null; then
        PYTHONPYCACHEPREFIX=/persist/python/pycache
    elif [ -d /scratch ] && /bin/busybox mkdir -p /scratch/python/pycache 2>/dev/null; then
        PYTHONPYCACHEPREFIX=/scratch/python/pycache
    else
        /bin/busybox mkdir -p /tmp/python/pycache 2>/dev/null || true
        PYTHONPYCACHEPREFIX=/tmp/python/pycache
    fi
fi

export CPYTHON_ROOT
export PYTHONHOME="$CPYTHON_ROOT/usr"
export PYTHONPYCACHEPREFIX
export PYTHONDONTWRITEBYTECODE="${PYTHONDONTWRITEBYTECODE:-0}"
export PYTHONUTF8="${PYTHONUTF8:-1}"
export LANG="${LANG:-C.UTF-8}"
export LC_ALL="${LC_ALL:-C.UTF-8}"
export SSL_CERT_FILE="${SSL_CERT_FILE:-$CPYTHON_ROOT/etc/ssl/certs/ca-certificates.crt}"

CPYTHON_LIBRARY_PATH="$CPYTHON_ROOT/usr/lib:$CPYTHON_ROOT/lib"
exec "$CPYTHON_LD" --library-path "$CPYTHON_LIBRARY_PATH" "$CPYTHON_PY" "$@"
