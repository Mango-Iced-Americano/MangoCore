ARCHS = rv64
CURR_ARCH ?= la64
FS_MODE ?= ext4
TOP_BLK_MODE_ORIGIN := $(origin BLK_MODE)
BLK_MODE ?= virt
LA64_BLK_MODE ?= $(if $(filter undefined,$(TOP_BLK_MODE_ORIGIN)),virt_pci,$(BLK_MODE))
MODE ?= release
KERNEL_OUTPUT_ROOT ?= target
CONF_FILE ?= ../os_test.conf
CONF_ARCH ?= $(CURR_ARCH)
CONF_BLK_MODE ?= $(if $(filter la la64,$(CONF_ARCH)),virt_pci,$(BLK_MODE))
CONF_IMAGE ?=
AUTO_REBUILD_MEM ?= 1

# ============================================================
# 工具盘构建 (disk.img / disk-la.img)
# 内容：busybox + bash + musl/glibc 库 + 软链接
# 来源：user/tools/{riscv64,loongarch64}/
# ============================================================

TOOLS_IMG_RV := ../disk.img
TOOLS_IMG_LA := ../disk-la.img
TOOLS_SIZE_RV := 2048
TOOLS_SIZE_LA := 2048
TOOLS_SRC_RV := ../user/tools/riscv64
TOOLS_SRC_LA := ../user/tools/loongarch64

# CPython runtime
CPYTHON_COMMON := ../user/tools/cpython
CPYTHON_SRC_RV := ../user/tools/riscv64/tests/cpython
CPYTHON_SRC_LA := ../user/tools/loongarch64/tests/cpython
CPYTHON_AUTO ?= 1

# MBR 脚本路径
MBR_SCRIPT := ../scripts/make_mbr_tools_disk.py

# ============================================================
# Alpine 预编译工具下载（本地缓存，避免每次下载）
# 下载到 user/tools/{arch}/，tools-disk 自动打包
# ============================================================

ALPINE_MIRROR := https://dl-cdn.alpinelinux.org/alpine/edge/main

# ============================================================
# Initramfs 构建
# 生成 tiny newc cpio 归档，嵌入 kernel 作为初始根文件系统
# ============================================================

INITRAMFS_DIR_RV := ../fs-img-dir/initramfs-rv.cpio
INITRAMFS_DIR_LA := ../fs-img-dir/initramfs-la.cpio

# DNS 服务器：QEMU user 网用 10.0.2.3，真机/其他环境可覆写
DNS_SERVER ?= 10.0.2.3

# ============================================================
# L3 Kernel self-test (mango.mode=ktest)
# ============================================================
# Usage:
#   make rv64-ktest
#   make rv64-ktest KTEST=waitqueue KREPEAT=100
#   make rv64-ktest KTEST=sched KTRACE=waitqueue,sched
#   make la64-ktest KTEST=all KTIMEOUT_MS=10000
KTEST ?= all
KREPEAT ?= 1
KTIMEOUT_MS ?= 5000
KTRACE ?=
KTEST_QEMU_TIMEOUT ?= 30
export KTEST_QEMU_TIMEOUT

KTEST_CMDLINE := mango.mode=ktest mango.test=$(KTEST) mango.test.repeat=$(KREPEAT) mango.test.timeout_ms=$(KTIMEOUT_MS) mango.test.failfast=0
ifneq ($(KTRACE),)
KTEST_CMDLINE := $(KTEST_CMDLINE) mango.trace=$(KTRACE)
endif
export KTEST_CMDLINE
