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
/bin/busybox grep -q '"runtime_interpreter": "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"' "$manifest"
/bin/busybox grep -q '"Pillow"' "$manifest"
/bin/busybox grep -q '"version": "12.3.0"' "$manifest"
/bin/busybox grep -q '"shared_soname": "libjpeg.so.62"' "$manifest"
/bin/busybox grep -q '"MarkupSafe"' "$manifest"
/bin/busybox grep -q '"version": "3.0.3"' "$manifest"
/bin/busybox grep -q '"PyYAML"' "$manifest"
/bin/busybox grep -q '"version": "6.0.3"' "$manifest"
/bin/busybox grep -q '"pure_python": true' "$manifest"

CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" -S \
    "$runtime_root/verify_runtime_integrity.py"
CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" -S -c \
    'import _bz2,_ctypes,_decimal,_hashlib,_lzma,_sqlite3,readline,ssl,sysconfig,threading,zlib;flags=" ".join(str(sysconfig.get_config_var(k) or "") for k in ("CFLAGS","CONFIGURE_CFLAGS","CONFIGURE_CFLAGS_NODIST","PY_CFLAGS","PGO_PROF_USE_FLAG"));assert "-mstrict-align" in flags and "-fprofile-use" in flags;args=sysconfig.get_config_var("CONFIG_ARGS") or "";assert "--enable-optimizations" in args and "--with-lto" in args;t=threading.Thread(target=lambda:None);t.start();t.join();print("strict-runtime-board-smoke-ok")'
CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" \
    "$runtime_root/pillow_strict_smoke.py"
CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" -c \
    'from markupsafe import Markup,_speedups,escape;assert escape("<x>")==Markup("&lt;x&gt;");print("strict-markupsafe-board-smoke-ok",_speedups.__file__)'
CPYTHON_ROOT="$runtime_root" "$runtime_root/python3-wrapper.sh" -c \
    'import yaml;assert yaml.__version__=="6.0.3" and yaml.__with_libyaml__ is False;assert yaml.safe_load("answer: 42")=={"answer":42};print("strict-pyyaml-pure-board-smoke-ok",yaml.__file__)'
