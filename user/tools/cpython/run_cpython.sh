#!/bin/sh
set -u

CPYTHON_ROOT="${CPYTHON_ROOT:-/tools/tests/cpython}"

# Detect the musl loader for the current architecture
if [ -x "$CPYTHON_ROOT/lib/ld-musl-riscv64.so.1" ]; then
    CPYTHON_LD="$CPYTHON_ROOT/lib/ld-musl-riscv64.so.1"
elif [ -x "$CPYTHON_ROOT/lib/ld-musl-loongarch64.so.1" ]; then
    CPYTHON_LD="$CPYTHON_ROOT/lib/ld-musl-loongarch64.so.1"
else
    echo "[CPYTHON ENV FAIL] musl loader not found under $CPYTHON_ROOT/lib"
    exit 127
fi

export CPYTHON_ROOT
export CPYTHON_PY="$CPYTHON_ROOT/usr/bin/python3"
export PYTHONHOME="$CPYTHON_ROOT/usr"
export PYTHONNOUSERSITE=1
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUTF8=1
export LANG=C.UTF-8
export LC_ALL=C.UTF-8
export PATH="$CPYTHON_ROOT/usr/bin:/bin:/sbin:/usr/bin:/usr/sbin"
export LD_LIBRARY_PATH="$CPYTHON_ROOT/usr/lib:$CPYTHON_ROOT/lib"
export SSL_CERT_FILE="$CPYTHON_ROOT/etc/ssl/certs/ca-certificates.crt"

if [ "${1:-}" = "--exec" ]; then
    shift
    exec "$CPYTHON_LD" --library-path "$LD_LIBRARY_PATH" "$CPYTHON_PY" "$@"
fi
