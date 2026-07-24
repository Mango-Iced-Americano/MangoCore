#!/bin/sh
export PATH=/usr/bin:/bin
exec /usr/bin/clang --target=loongarch64-linux-gnu -fuse-ld=lld "$@"
