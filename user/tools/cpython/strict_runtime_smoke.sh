#!/bin/sh
# Validate the staged runtime before the P4 same-filesystem publish step.
set -eu

runtime_root=$(/bin/busybox dirname "$0")
runtime_root=$(cd "$runtime_root" && pwd)
manifest=$runtime_root/strict-runtime-manifest.json

test -f "$manifest"
/bin/busybox grep -q -- -mstrict-align "$manifest"
/bin/busybox grep -q '"target": "loongarch64-linux-musl"' "$manifest"
/bin/busybox grep -q '"pgo": true' "$manifest"
/bin/busybox grep -q '"lto": true' "$manifest"
/bin/busybox grep -q '"kernel_handler_modified": false' "$manifest"

CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" -S -c \
    'import _bz2,_ctypes,_decimal,_hashlib,_lzma,_sqlite3,readline,ssl,sysconfig,threading,zlib;flags=" ".join(str(sysconfig.get_config_var(k) or "") for k in ("CFLAGS","CONFIGURE_CFLAGS","CONFIGURE_CFLAGS_NODIST","PY_CFLAGS","PGO_PROF_USE_FLAG"));assert "-mstrict-align" in flags and "-fprofile-use" in flags;args=sysconfig.get_config_var("CONFIG_ARGS") or "";assert "--enable-optimizations" in args and "--with-lto" in args;t=threading.Thread(target=lambda:None);t.start();t.join();print("strict-runtime-board-smoke-ok")'
