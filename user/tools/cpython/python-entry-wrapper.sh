#!/bin/sh
set -eu

# Run Python console entry points through the P4 strict-aligned interpreter,
# ignoring any stale shebang written by an older pip/runtime.
entry=${0##*/}
launcher=/rescue/python3-wrapper
if [ ! -x "$launcher" ]; then
    launcher=/usr/bin/python3
fi

case "$entry" in
    pip|pip3)
        exec "$launcher" -m pip "$@"
        ;;
    *)
        script=
        for candidate in \
            "/persist/python/user/bin/$entry" \
            "/var/cache/mango-python/user/bin/$entry"; do
            if [ -f "$candidate" ]; then
                # Earlier chroot provisioning kept pip's original Python
                # console script as `<name>.real` and placed a shell shim at
                # `<name>`.  Ignore that shim (and its stale shebang/path) and
                # route the original source through the strict launcher.
                if [ -f "$candidate.real" ]; then
                    candidate="$candidate.real"
                fi
                resolved=$(/bin/busybox readlink -f "$candidate" 2>/dev/null || true)
                case "$resolved" in
                    /persist/python/user/bin/*|/var/cache/mango-python/user/bin/*)
                        script=$resolved
                        break
                        ;;
                    *)
                        echo "$entry: refusing console entry point outside the P4 Python user base: $resolved" >&2
                        exit 126
                        ;;
                esac
            fi
        done
        if [ -z "$script" ]; then
            echo "$entry: console entry point is not installed in the P4 Python user base" >&2
            exit 127
        fi
        exec "$launcher" "$script" "$@"
        ;;
esac
