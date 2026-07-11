#!/bin/sh
# build_initramfs.sh — 构建 tiny initramfs newc cpio 归档
#
# 用法: ./build_initramfs.sh <arch> <mode> <output_path> [profile]
#   arch: rv64 | la64
#   mode: release | debug
#   output_path: 生成的 cpio 路径 (如 ../fs-img-dir/initramfs-rv.cpio)
#   profile: (可选) "regression" 构建最小回归测试 initramfs

set -eu

ARCH="$1"
MODE="${2:-release}"
OUT="$3"
PROFILE="${4:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
STAGE="$(mktemp -d)"
# 将输出路径转为绝对路径
case "$OUT" in
  /*) OUT_ABS="$OUT" ;;
  *)  OUT_ABS="$PWD/$OUT" ;;
esac

if [ "$PROFILE" = "regression" ]; then
    echo "[initramfs] Building regression initramfs for $ARCH ($MODE)..."

    # Copy common skeleton (needed for /dev, /etc, /tmp dirs etc.)
    cp -a "$SCRIPT_DIR/initramfs/common/." "$STAGE/"
    # Remove files we don't need in regression mode
    rm -rf "$STAGE/bin" "$STAGE/rescue" "$STAGE/apk" 2>/dev/null || true

    # Determine arch-specific paths
    case "$ARCH" in
      rv64)
        INIT_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/regression_init"
        REG_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/regression"
        ;;
      la64)
        INIT_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/regression_init"
        REG_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/regression"
        ;;
      *)
        echo "[initramfs] ERROR: unknown arch: $ARCH"
        rm -rf "$STAGE"
        exit 1
        ;;
    esac

    # 3. 安装 /init（regression_init 作为 PID1）
    if [ -f "$INIT_SRC" ]; then
        install -m 0755 "$INIT_SRC" "$STAGE/init"
        echo "[initramfs] installed /init from $INIT_SRC"
    else
        echo "[initramfs] ERROR: $INIT_SRC not found"
        rm -rf "$STAGE"
        exit 1
    fi

    # 4. 安装 /regression（回归测试二进制）
    if [ -f "$REG_SRC" ]; then
        install -m 0755 "$REG_SRC" "$STAGE/regression"
        echo "[initramfs] installed /regression from $REG_SRC"
    else
        echo "[initramfs] ERROR: $REG_SRC not found"
        rm -rf "$STAGE"
        exit 1
    fi

else
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
fi

# 5. 生成 newc cpio 归档
(
    cd "$STAGE"
    find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc -R 0:0 > "$OUT_ABS" 2>/dev/null
)

echo "[initramfs] generated $OUT_ABS ($(du -h "$OUT_ABS" | cut -f1))"
rm -rf "$STAGE"
