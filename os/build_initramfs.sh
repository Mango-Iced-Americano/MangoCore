#!/bin/sh
# build_initramfs.sh — 构建 tiny initramfs newc cpio 归档
#
# 用法: ./build_initramfs.sh <arch> <mode> <output_path>
#   arch: rv64 | la64
#   mode: release | debug
#   output_path: 生成的 cpio 路径 (如 ../fs-img-dir/initramfs-rv.cpio)

set -eu

ARCH="$1"
MODE="${2:-release}"
OUT="$3"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
STAGE="$(mktemp -d)"
# 将输出路径转为绝对路径
case "$OUT" in
  /*) OUT_ABS="$OUT" ;;
  *)  OUT_ABS="$PWD/$OUT" ;;
esac

echo "[initramfs] Building initramfs for $ARCH ($MODE)..."

# 1. 复制 common skeleton
cp -a "$SCRIPT_DIR/initramfs/common/." "$STAGE/"

# 1b. 生成 /etc/resolv.conf（DNS_SERVER 环境变量可配置，默认 10.0.2.3 适配 QEMU user 网）
DNS_SERVER="${DNS_SERVER:-10.0.2.3}"
printf 'nameserver %s\n' "$DNS_SERVER" > "$STAGE/etc/resolv.conf"

# 2. 确定架构相关的路径
case "$ARCH" in
  rv64)
    INIT_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/init"
    BUSYBOX_SRC="../user/tools/riscv64/bin/busybox"
    INET_TEST_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/inet_test"
    ;;
  la64)
    INIT_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/init"
    BUSYBOX_SRC="../user/tools/loongarch64/bin/busybox"
    INET_TEST_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/inet_test"
    ;;
  *)
    echo "[initramfs] ERROR: unknown arch: $ARCH"
    rm -rf "$STAGE"
    exit 1
    ;;
esac

# Optional self-contained curl runtime for the 2K1000 DHCP shell image.
if [ "${CURL_RUNTIME:-0}" = "1" ]; then
    if [ "$ARCH" != "la64" ]; then
        echo "[initramfs] ERROR: curl runtime is currently available only for la64"
        rm -rf "$STAGE"
        exit 1
    fi
    CURL_SRC="../user/tools/loongarch64/curl-runtime"
    if [ ! -x "$CURL_SRC/bin/curl" ]; then
        echo "[initramfs] ERROR: missing curl runtime; run make tools-curl-la"
        rm -rf "$STAGE"
        exit 1
    fi
    cp -a "$CURL_SRC/bin/." "$STAGE/bin/"
    if [ -d "$CURL_SRC/lib" ]; then
        cp -a "$CURL_SRC/lib/." "$STAGE/lib/"
    fi
    if [ -d "$CURL_SRC/lib64" ]; then
        mkdir -p "$STAGE/lib64"
        cp -a "$CURL_SRC/lib64/." "$STAGE/lib64/"
    fi
    cp -a "$CURL_SRC/etc/." "$STAGE/etc/"
    echo "[initramfs] installed self-contained LoongArch64 curl runtime"
fi

# Optional test binary pinned to the same source revision as the kernel.
# Install outside /tests because board startup bind-mounts the SSD tools tree
# there, which may contain an older inet_test.
if [ "${INET_TEST_RUNTIME:-0}" = "1" ]; then
    if [ ! -x "$INET_TEST_SRC" ]; then
        echo "[initramfs] ERROR: missing inet_test build: $INET_TEST_SRC"
        rm -rf "$STAGE"
        exit 1
    fi
    install -m 0755 "$INET_TEST_SRC" "$STAGE/bin/inet_test"
    echo "[initramfs] installed current inet_test at /bin/inet_test"
fi

# 3. 安装 /init（从 initproc 构建产物）— stage-1 引导入口
if [ -f "$INIT_SRC" ]; then
    install -m 0755 "$INIT_SRC" "$STAGE/init"
    echo "[initramfs] installed /init from $INIT_SRC"
else
    echo "[initramfs] WARNING: $INIT_SRC not found, /init will be missing"
fi

# 4. 安装 /rescue/sh（静态 BusyBox，救援 shell）
if [ -f "$BUSYBOX_SRC" ]; then
    mkdir -p "$STAGE/rescue"
    install -m 0755 "$BUSYBOX_SRC" "$STAGE/rescue/sh"
    echo "[initramfs] installed /rescue/sh from $BUSYBOX_SRC"
else
    echo "[initramfs] WARNING: $BUSYBOX_SRC not found, /rescue/sh will be missing"
fi

# 5. 生成 newc cpio 归档
(
    cd "$STAGE"
    find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc -R 0:0 > "$OUT_ABS" 2>/dev/null
)

echo "[initramfs] generated $OUT_ABS ($(du -h "$OUT_ABS" | cut -f1))"
rm -rf "$STAGE"
