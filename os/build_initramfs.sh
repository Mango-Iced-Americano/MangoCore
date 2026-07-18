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
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM
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

    # NTP can be unavailable during early bring-up. Record a reproducible lower
    # bound for the wall clock so TLS validation never uses a stale fixed date.
    BUILD_EPOCH="${SOURCE_DATE_EPOCH:-$(date -u +%s)}"
    case "$BUILD_EPOCH" in
      ''|*[!0-9]*)
        echo "[initramfs] ERROR: invalid SOURCE_DATE_EPOCH: $BUILD_EPOCH"
        rm -rf "$STAGE"
        exit 1
        ;;
    esac
    printf '%s\n' "$BUILD_EPOCH" > "$STAGE/etc/build-epoch"

    # 2. 确定架构相关的路径
    case "$ARCH" in
      rv64)
        INIT_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/init"
        BUSYBOX_SRC="../user/tools/riscv64/bin/busybox"
        APK_SRC="../user/tools/riscv64/bin/apk.static"
        INET_TEST_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/inet_test"
        RNG_TEST_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/rng_test"
        REG_SRC="../user/target/riscv64gc-unknown-none-elf/$MODE/regression"
        ;;
      la64)
        INIT_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/init"
        BUSYBOX_SRC="../user/tools/loongarch64/bin/busybox"
        APK_SRC="../user/tools/loongarch64/bin/apk.static"
        INET_TEST_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/inet_test"
        RNG_TEST_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/rng_test"
        REG_SRC="../user/target/loongarch64-unknown-linux-gnu/$MODE/regression"
        ;;
      *)
        echo "[initramfs] ERROR: unknown arch: $ARCH"
        rm -rf "$STAGE"
        exit 1
        ;;
    esac

    # Optional package-manager runtime used by isolated APK gates.
    if [ "${APK_RUNTIME:-0}" = "1" ]; then
        if [ ! -x "$APK_SRC" ]; then
            echo "[initramfs] ERROR: missing apk runtime; run make tools-apk"
            rm -rf "$STAGE"
            exit 1
        fi
        install -m 0755 "$APK_SRC" "$STAGE/bin/apk.static"
        cp -a "$SCRIPT_DIR/initramfs/apk/." "$STAGE/"
        echo "[initramfs] installed self-contained $ARCH apk.static"
    fi

    # Optional self-contained HTTPS curl runtime for QEMU and 2K1000 shells.
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

    # Keep opt-in board diagnostics pinned to the same source revision as the
    # kernel instead of taking potentially stale copies from the tools disk.
    if [ "${INET_TEST_RUNTIME:-0}" = "1" ]; then
        if [ ! -x "$INET_TEST_SRC" ]; then
            echo "[initramfs] ERROR: missing inet_test build: $INET_TEST_SRC"
            rm -rf "$STAGE"
            exit 1
        fi
        install -m 0755 "$INET_TEST_SRC" "$STAGE/bin/inet_test"
        echo "[initramfs] installed current inet_test at /bin/inet_test"
    fi

    if [ "${RNG_TEST_RUNTIME:-0}" = "1" ]; then
        if [ ! -x "$RNG_TEST_SRC" ]; then
            echo "[initramfs] ERROR: missing rng_test build: $RNG_TEST_SRC"
            rm -rf "$STAGE"
            exit 1
        fi
        install -m 0755 "$RNG_TEST_SRC" "$STAGE/bin/rng_test"
        echo "[initramfs] installed current rng_test at /bin/rng_test"
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

    # 5. 安装 /regression（normal initproc 的 mode=regression 路径）
    if [ -f "$REG_SRC" ]; then
        install -m 0755 "$REG_SRC" "$STAGE/regression"
        echo "[initramfs] installed /regression from $REG_SRC"
    else
        echo "[initramfs] WARNING: $REG_SRC not found, /regression will be missing"
    fi

    # RV64 keeps its optional isolated-runtime launcher. The strict LoongArch
    # helpers are P4-specific and must not become prerequisites of ordinary
    # QEMU, regression, or non-persistent board images.
    if [ "$ARCH" = "rv64" ]; then
        CPYTHON_WRAPPER_SRC="../user/tools/cpython/python3-wrapper.sh"
        if [ -x "$CPYTHON_WRAPPER_SRC" ]; then
            mkdir -p "$STAGE/rescue"
            install -m 0755 "$CPYTHON_WRAPPER_SRC" "$STAGE/rescue/python3-wrapper"
            echo "[initramfs] installed RV64 CPython wrapper at /rescue/python3-wrapper"
        fi
    elif [ "${PERSIST_PYTHON_RUNTIME:-0}" = "1" ]; then
        CPYTHON_WRAPPER_SRC="../user/tools/cpython/python3-wrapper-persist.sh"
        CPYTHON_ENTRY_SRC="../user/tools/cpython/python-entry-wrapper.sh"
        CPYTHON_VERIFY_SRC="../scripts/board/verify_persist_python.sh"
        SMOLAGENTS_PATCH_SRC="../scripts/board/patch_smolagents_action_type.py"
        DDGS_PATCH_SRC="../scripts/board/patch_ddgs_redirect.py"

        for required_file in \
            "$CPYTHON_WRAPPER_SRC" \
            "$CPYTHON_ENTRY_SRC" \
            "$CPYTHON_VERIFY_SRC" \
            "$SMOLAGENTS_PATCH_SRC" \
            "$DDGS_PATCH_SRC"; do
            if [ ! -f "$required_file" ]; then
                echo "[initramfs] ERROR: missing required P4 Python resource: $required_file" >&2
                exit 1
            fi
        done

        mkdir -p "$STAGE/rescue"
        install -m 0755 "$CPYTHON_WRAPPER_SRC" "$STAGE/rescue/python3-wrapper"
        install -m 0755 "$CPYTHON_ENTRY_SRC" "$STAGE/rescue/python-entry"
        install -m 0755 "$CPYTHON_VERIFY_SRC" "$STAGE/rescue/verify-persist-python"
        install -m 0755 "$SMOLAGENTS_PATCH_SRC" "$STAGE/rescue/patch-smolagents-action-type"
        install -m 0755 "$DDGS_PATCH_SRC" "$STAGE/rescue/patch-ddgs-redirect"
        echo "[initramfs] installed strict P4 Python launch and verification resources"
    fi

fi

# 6. 生成 newc cpio 归档
(
    cd "$STAGE"
    find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc -R 0:0 > "$OUT_ABS" 2>/dev/null
)

echo "[initramfs] generated $OUT_ABS ($(du -h "$OUT_ABS" | cut -f1))"
rm -rf "$STAGE"
