#!/bin/sh
set -u

fail=0

check_file() {
    item="$1" path="$2"
    if [ -f "$path" ] && [ -s "$path" ]; then
        echo "[CPYTHON L3] check: $item OK"
    else
        echo "[CPYTHON L3] FAIL: $item (missing or empty: $path)"
        fail=1
    fi
}

check_exec() {
    item="$1" path="$2"
    if [ -x "$path" ] && [ -s "$path" ]; then
        echo "[CPYTHON L3] check: $item OK"
    else
        echo "[CPYTHON L3] FAIL: $item (missing or not executable: $path)"
        fail=1
    fi
}

check_dir() {
    item="$1" path="$2"
    if [ -d "$path" ]; then
        echo "[CPYTHON L3] check: $item OK"
    else
        echo "[CPYTHON L3] FAIL: $item (missing dir: $path)"
        fail=1
    fi
}

# Source the environment
. /tools/tests/cpython/run_cpython.sh

check_exec "python3 binary" "$CPYTHON_PY"
check_exec "musl loader" "$CPYTHON_LD"
check_dir   "usr/lib" "$CPYTHON_ROOT/usr/lib"

# Encodings - CPython is fatal without this
enc_path=$(ls "$CPYTHON_ROOT/usr/lib/python3."*"/encodings/__init__.py" 2>/dev/null | head -1)
if [ -n "$enc_path" ] && [ -s "$enc_path" ]; then
    echo "[CPYTHON L3] check: encodings OK"
else
    echo "[CPYTHON L3] FAIL: encodings (no python3.*/encodings/__init__.py found)"
    fail=1
fi

check_file "CA certs" "$CPYTHON_ROOT/etc/ssl/certs/ca-certificates.crt"

# Check .so files required for startup
libpy_found=0
for f in "$CPYTHON_ROOT/usr/lib/libpython3."*".so"*; do
    if [ -f "$f" ] && [ -s "$f" ]; then
        echo "[CPYTHON L3] check: lib $(basename "$f") OK"
        libpy_found=1
    elif [ -e "$f" ] && [ ! -s "$f" ]; then
        echo "[CPYTHON L3] FAIL: lib $(basename "$f") empty"
        fail=1
    fi
done
[ "$libpy_found" -eq 1 ] || { echo "[CPYTHON L3] FAIL: libpython3.*.so not found"; fail=1; }

libc_found=0
for f in "$CPYTHON_ROOT/lib/libc.so"* "$CPYTHON_ROOT/lib/libc.musl-"*".so"* "$CPYTHON_ROOT/lib/ld-musl-"*".so.1"; do
    if [ -f "$f" ] && [ -s "$f" ]; then
        echo "[CPYTHON L3] check: lib $(basename "$f") OK"
        libc_found=1
    elif [ -e "$f" ] && [ ! -s "$f" ]; then
        echo "[CPYTHON L3] FAIL: lib $(basename "$f") empty"
        fail=1
    fi
done
[ "$libc_found" -eq 1 ] || { echo "[CPYTHON L3] FAIL: musl libc/loader not found"; fail=1; }

for soname in libcrypto.so libssl.so; do
    found=0
    for f in "$CPYTHON_ROOT/usr/lib/$soname"*; do
        if [ -f "$f" ] && [ -s "$f" ]; then
            echo "[CPYTHON L3] check: lib $(basename "$f") OK"
            found=1
        elif [ -e "$f" ] && [ ! -s "$f" ]; then
            echo "[CPYTHON L3] FAIL: lib $(basename "$f") empty"
            fail=1
        fi
    done
    [ "$found" -eq 1 ] || { echo "[CPYTHON L3] FAIL: $soname not found"; fail=1; }
done

if [ "$fail" -eq 0 ]; then
    echo "[CPYTHON L3] files OK"
else
    echo "[CPYTHON L3] files FAIL"
fi

exit "$fail"
