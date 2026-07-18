#!/bin/sh
set -eu

# LoongArch production policy: Python is allowed to run only from the
# strict-aligned runtime published on P4 ext4.  /tools is deliberately not a
# fallback so a missing, stale, or corrupt P4 runtime remains visible.
RUNTIME_BASE=/persist/python-runtime
CPYTHON_ROOT="${CPYTHON_ROOT:-$RUNTIME_BASE/current}"
BUSYBOX=/bin/busybox

case "$CPYTHON_ROOT" in
    "$RUNTIME_BASE/current"|"$RUNTIME_BASE/releases/"*) ;;
    *)
        echo "python3: refusing non-P4 runtime: $CPYTHON_ROOT" >&2
        exit 126
        ;;
esac

resolved_root=$($BUSYBOX readlink -f "$CPYTHON_ROOT" 2>/dev/null || true)
case "$resolved_root" in
    "$RUNTIME_BASE/releases/"*) ;;
    *)
        echo "python3: P4 runtime is not an active release: $CPYTHON_ROOT" >&2
        exit 127
        ;;
esac
CPYTHON_ROOT=$resolved_root

manifest=$CPYTHON_ROOT/strict-runtime-manifest.json
activation=$CPYTHON_ROOT/.mango-strict-runtime
if [ ! -r "$manifest" ] || [ ! -r "$activation" ]; then
    echo "python3: strict runtime manifest/activation is missing under $CPYTHON_ROOT" >&2
    exit 127
fi
manifest_line=$($BUSYBOX sha256sum "$manifest")
manifest_sha=${manifest_line%% *}
if ! $BUSYBOX grep -Fxq "schema=1" "$activation" || \
   ! $BUSYBOX grep -Fxq "runtime_policy=mangocore-la64-strict-align-v1" "$activation" || \
   ! $BUSYBOX grep -Fxq "manifest_sha256=$manifest_sha" "$activation"; then
    echo "python3: strict runtime activation does not match its manifest" >&2
    exit 126
fi
artifact_sha=$($BUSYBOX sed -n 's/^artifact_sha256=//p' "$activation")
release_id=${CPYTHON_ROOT##*/}
case "$artifact_sha" in
    "$release_id"????????????????????????????????????????????????????) ;;
    *)
        echo "python3: strict runtime activation does not match release $release_id" >&2
        exit 126
        ;;
esac
if ! $BUSYBOX grep -Fq '"target": "loongarch64-linux-musl"' "$manifest" || \
   ! $BUSYBOX grep -Fq '"strict_flags": "-march=loongarch64 -mabi=lp64d -mstrict-align"' "$manifest" || \
   ! $BUSYBOX grep -Fq '"runtime_interpreter": "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"' "$manifest"; then
    echo "python3: runtime is not the approved strict-aligned LoongArch build" >&2
    exit 126
fi

CPYTHON_LD=$CPYTHON_ROOT/lib/ld-musl-loongarch64.so.1
CPYTHON_PY=$CPYTHON_ROOT/usr/bin/python3
if [ ! -x "$CPYTHON_LD" ] || [ ! -x "$CPYTHON_PY" ]; then
    echo "python3: strict runtime is incomplete under $CPYTHON_ROOT" >&2
    exit 127
fi

# All mutable Python state also stays on P4 ext4.  There is intentionally no
# scratch/tmpfs fallback because that would hide P4 mount and persistence bugs.
PYTHON_STATE=/persist/python
$BUSYBOX mkdir -p \
    "$PYTHON_STATE/tmp" \
    "$PYTHON_STATE/user" \
    "$PYTHON_STATE/pycache"
if [ ! -d "$PYTHON_STATE/tmp" ] || [ ! -d "$PYTHON_STATE/user" ] || \
   [ ! -d "$PYTHON_STATE/pycache" ]; then
    echo "python3: P4 Python state directories are unavailable" >&2
    exit 127
fi

export CPYTHON_ROOT
export CPYTHON_LD
export CPYTHON_PY
export PYTHONHOME="$CPYTHON_ROOT/usr"
export PYTHONUSERBASE="$PYTHON_STATE/user"
export PYTHONPYCACHEPREFIX="$PYTHON_STATE/pycache"
export TMPDIR="$PYTHON_STATE/tmp"
export PYTHONDONTWRITEBYTECODE="${PYTHONDONTWRITEBYTECODE:-0}"
export PYTHONUTF8="${PYTHONUTF8:-1}"
export LANG="${LANG:-C.UTF-8}"
export LC_ALL="${LC_ALL:-C.UTF-8}"
export SSL_CERT_FILE="$CPYTHON_ROOT/etc/ssl/certs/ca-certificates.crt"
export OPENSSL_CONF="$CPYTHON_ROOT/etc/ssl/openssl.cnf"
export OPENSSL_MODULES="$CPYTHON_ROOT/usr/lib/ossl-modules"
export MANGO_PYTHON_POLICY=p4-strict-align-v1
export PIP_USER=1
export PIP_CACHE_DIR="$PYTHON_STATE/pip-cache"
export PIP_CONFIG_FILE=/dev/null
$BUSYBOX mkdir -p "$PIP_CACHE_DIR"

# Do not let an inherited host/P3 environment inject Python modules, startup
# code, native DSOs, OpenSSL providers, or locale objects before the strict
# runtime has established its own paths.  These variables are deliberately
# removed instead of filtered: an incomplete filter would hide mixed-runtime
# defects that this launcher is intended to expose.
unset PYTHONPATH PYTHONSTARTUP PYTHONINSPECT PYTHONBREAKPOINT
unset PYTHONPLATLIBDIR PYTHONEXECUTABLE
unset LD_PRELOAD LD_AUDIT MUSL_LOCPATH SSL_CERT_DIR

# Do not inherit /tools library or executable paths into Python descendants.
# Console entry points are routed through /usr/bin wrappers below.
export LD_LIBRARY_PATH="$CPYTHON_ROOT/usr/lib:$CPYTHON_ROOT/lib"
export PATH="/bin:/sbin:/usr/bin:/usr/sbin"

exec "$CPYTHON_LD" \
    --library-path "$LD_LIBRARY_PATH" \
    "$CPYTHON_PY" "$@"
