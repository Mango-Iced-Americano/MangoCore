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

# 2. 确定架构相关的路径
case "$ARCH" in
  rv64)
    INIT_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/init"
    BUSYBOX_SRC="../user/tools/riscv64/bin/busybox"
    ;;
  la64)
    INIT_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/init"
    BUSYBOX_SRC="../user/tools/loongarch64/bin/busybox"
    ;;
  *)
    echo "[initramfs] ERROR: unknown arch: $ARCH"
    rm -rf "$STAGE"
    exit 1
    ;;
esac

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
