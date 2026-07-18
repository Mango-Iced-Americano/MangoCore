#!/bin/sh
set -eu

fail() {
    echo "[persist-python-verify] FAIL: $*" >&2
    exit 1
}

require_smolagents=0
if [ "${1:-}" = "--require-smolagents" ]; then
    require_smolagents=1
elif [ "$#" -ne 0 ]; then
    fail "usage: $0 [--require-smolagents]"
fi

/bin/busybox grep -Eq \
    '^/persist[[:space:]]+/persist[[:space:]]+ext4[[:space:]]+rw([,[:space:]])' \
    /proc/mounts || fail "/persist is not P4 ext4 rw"
test ! -L /persist || fail "/persist must not be a symlink"

current=/persist/python-runtime/current
release=$(/bin/busybox readlink -f "$current" 2>/dev/null || true)
case "$release" in
    /persist/python-runtime/releases/????????????) ;;
    *) fail "current does not resolve to a strict release: $release" ;;
esac
test -r "$release/.mango-strict-runtime" || fail "activation marker is missing"
test -r "$release/strict-runtime-manifest.json" || fail "manifest is missing"
/bin/busybox grep -Fq \
    '"runtime_interpreter": "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"' \
    "$release/strict-runtime-manifest.json" \
    || fail "runtime PT_INTERP is not bound to the P4 current loader"

for name in python python3; do
    test "$(command -v "$name")" = "/usr/bin/$name" \
        || fail "$name does not resolve through /usr/bin"
    test "$(/bin/busybox readlink -f "/usr/bin/$name")" = /rescue/python3-wrapper \
        || fail "$name bypasses the strict wrapper"
done
for name in pip pip3 smolagent smolagents; do
    test "$(command -v "$name")" = "/usr/bin/$name" \
        || fail "$name does not resolve through /usr/bin"
    test "$(/bin/busybox readlink -f "/usr/bin/$name")" = /rescue/python-entry \
        || fail "$name bypasses the console-entry wrapper"
done

python3 -S -c '
import os, sys
root = os.environ.get("CPYTHON_ROOT", "")
assert root.startswith("/persist/python-runtime/releases/"), root
assert os.environ.get("MANGO_PYTHON_POLICY") == "p4-strict-align-v1"
assert os.environ.get("PYTHONUSERBASE") == "/persist/python/user"
assert os.environ.get("PYTHONPYCACHEPREFIX") == "/persist/python/pycache"
assert os.environ.get("OPENSSL_CONF", "").startswith(root), os.environ.get("OPENSSL_CONF")
assert os.environ.get("OPENSSL_MODULES", "").startswith(root), os.environ.get("OPENSSL_MODULES")
for name in ("PYTHONPATH", "PYTHONSTARTUP", "PYTHONINSPECT", "PYTHONBREAKPOINT",
             "PYTHONPLATLIBDIR", "PYTHONEXECUTABLE", "LD_PRELOAD", "LD_AUDIT",
             "MUSL_LOCPATH", "SSL_CERT_DIR"):
    assert name not in os.environ, (name, os.environ.get(name))
for value in (sys.executable, sys.prefix, os.environ.get("PATH", ""),
              os.environ.get("LD_LIBRARY_PATH", "")):
    assert "/tools" not in value, value
assert all("/tools" not in item for item in sys.path), sys.path
print("[persist-python-verify] interpreter=" + sys.executable)
print("[persist-python-verify] root=" + root)
'
python -S -c 'import os; assert os.environ["MANGO_PYTHON_POLICY"] == "p4-strict-align-v1"'
python3 -c '
import os, sys
assert all("/tools" not in item for item in sys.path), sys.path
assert os.environ.get("PYTHONUSERBASE") == "/persist/python/user"
print("[persist-python-verify] normal_site=pass")
'
python3 -S -c '
import subprocess, sys
child = subprocess.run(
    [sys.executable, "-S", "-c", "import os,sys;assert os.environ.get(\"MANGO_PYTHON_POLICY\")==\"p4-strict-align-v1\";print(sys.executable)"],
    check=True, text=True, capture_output=True)
assert child.stdout.strip().startswith("/persist/python-runtime/releases/"), child.stdout
print("[persist-python-verify] self_exec=" + child.stdout.strip())
'
python3 -m pip --version
python3 "$release/pillow_strict_smoke.py"
python3 "$release/smolagents_toolkit_smoke.py"

native_user=$(/bin/busybox find /persist/python/user -type f \
    \( -name '*.so' -o -name '*.so.*' \) -print -quit 2>/dev/null || true)
test -z "$native_user" \
    || fail "unmanifested native extension exists in the P4 user site: $native_user"

smolagents_package=/persist/python/user/lib/python3.14/site-packages/smolagents
if [ -d "$smolagents_package" ]; then
    python3 -S /rescue/patch-smolagents-action-type --check \
        || fail "smolagents source/cache integrity gate failed"
    echo "[persist-python-verify] smolagents_action_type=pass"
    if python3 -c 'import smolagents; print("[persist-python-verify] smolagents=" + smolagents.__file__)'; then
        echo "[persist-python-verify] smolagents_import=pass"
        if [ "$require_smolagents" = 1 ]; then
            smolagent --help >/dev/null
            echo "[persist-python-verify] smolagent_command=pass"
            python3 -c '
from smolagents.cli import TOOL_MAPPING
expected = {"python_interpreter", "web_search", "visit_webpage"}
assert expected <= TOOL_MAPPING.keys(), TOOL_MAPPING.keys()
instances = {name: TOOL_MAPPING[name]() for name in sorted(expected)}
assert all(instances.values())
print("[persist-python-verify] smolagents_builtin_tools=pass " + ",".join(instances))
'
        fi
    elif [ "$require_smolagents" = 1 ]; then
        fail "smolagents import failed under the strict runtime"
    else
        echo "[persist-python-verify] smolagents_import=failed-exposed" >&2
    fi
elif [ "$require_smolagents" = 1 ]; then
    fail "smolagents is missing under the strict runtime"
else
    echo "[persist-python-verify] smolagents_import=missing"
fi

echo "[persist-python-verify] PASS policy=p4-strict-align-v1 release=$release"
