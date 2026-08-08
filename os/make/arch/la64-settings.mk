# Building
BUILD_ROOT ?= $(abspath ../build)
PROFILE ?= normal
TARGET := loongarch64-unknown-linux-gnu
MODE := release
PRODUCT_ROOT ?= $(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)
KERNEL_OUTPUT_ROOT ?= $(PRODUCT_ROOT)/kernel
USER_OUTPUT_ROOT ?= $(PRODUCT_ROOT)/user
KERNEL_ELF := $(KERNEL_OUTPUT_ROOT)/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
KERNEL_UIMG := $(KERNEL_ELF).ui
BLK_MODE := virt_pci
BLK_DEV_x0 = -device virtio-blk-pci,drive=x0
BLK_DEV_x1 = -device virtio-blk-pci,drive=x1
NET_DEV = -device virtio-net-pci,netdev=net0 -netdev user,id=net0
FS_MODE ?= ext4
IMAGE_ROLE_LA64_PRODUCT_ROOT := $(PRODUCT_ROOT)
CORE_NUM := 1
LOG ?= off
KERNEL_LA := $(PRODUCT_ROOT)/kernel/kernel-la
SDCARD_LA := $(IMAGE_ROLE_LA64_COMPETITION_X0)
DISK_LA := $(IMAGE_ROLE_LA64_X1)

# ============================================================
# lwext4 C library (la64: uses pre-installed cross-compiler)
# ============================================================
LWEXT4_LA_DIR := ../dependency/lwext4_rust/c/lwext4
LWEXT4_LA_OUTPUT_DIR := $(KERNEL_OUTPUT_ROOT)/lwext4/la64
LWEXT4_LA_SOURCE_DIR := $(LWEXT4_LA_OUTPUT_DIR)/source
LWEXT4_LA_BUILD_DIR := $(LWEXT4_LA_OUTPUT_DIR)/build
LWEXT4_LA_LIB := $(LWEXT4_LA_OUTPUT_DIR)/liblwext4-loongarch64.a
LWEXT4_LA_PREPARED := $(LWEXT4_LA_OUTPUT_DIR)/.prepared
LWEXT4_LA_TOOLCHAIN_PATH := /opt/gcc-13.2.0-loongarch64-linux-gnu/bin
LWEXT4_LA_CMAKE := ../dependency/lwext4_rust/c/elf-linux-gnu.cmake
# Prerequisites for archive rebuild invalidation
LWEXT4_LA_INPUTS := $(shell find "$(LWEXT4_LA_DIR)" -type f ! -path "$(LWEXT4_LA_DIR)/build_*/*") \
                    $(LWEXT4_LA_CMAKE) \
                    ../dependency/lwext4_rust/c/ulibc.c

# BOARD
BOARD ?= laqemu
ifeq ($(BOARD),laqemu)
BOOT_PROFILE := boot_la_qemu
else ifeq ($(BOARD),2k1000)
BOOT_PROFILE := boot_la_uboot_dmw
else
$(error BOARD must be laqemu or 2k1000 for la64)
endif
LINKER_SCRIPT := src/hal/arch/loongarch64/linker-$(BOARD).ld

# Logging
ifndef LOG
	LOG_OPTION := "log_off"
else
	LOG_OPTION := "log_${LOG}"
endif

# Kernel entry and uImage addresses are board ABI, not QEMU defaults.  Keep
# the linker input immutable and select it through Cargo flags in la64.mk.
ifeq ($(BOARD),2k1000)
KERNEL_ENTRY_PA := 0x90000000
LA_LOAD_ADDR := 0x90000000
LA_ENTRY_POINT := 0x90000000
else
KERNEL_ENTRY_PA := 0x9000000090000000
LA_LOAD_ADDR := 0x9000000090000000
LA_ENTRY_POINT := 0x9000000090000000
endif

# Binutils (cross toolchain, not llvm)
OBJCOPY := loongarch64-linux-gnu-objcopy
OBJDUMP := loongarch64-linux-gnu-objdump
READELF := loongarch64-linux-gnu-readelf

# Applications
APPS := ../user/src/bin/*

# RootFS image
ifeq ($(BOARD), laqemu)
	ROOTFS_IMG := $(IMAGE_ROLE_LA64_DEVELOPMENT_X0)
endif

# Initramfs cpio generation — parameterized for normal / regression profiles
INITRAMFS_CPIO_LA := $(IMAGE_ROLE_LA64_BOOTSTRAP_ROOT)
REGRESSION_CPIO_LA := $(PRODUCT_ROOT)/initramfs/initramfs-regression-la.cpio
KTEST_CPIO_LA := $(PRODUCT_ROOT)/initramfs/initramfs-ktest-la.cpio

INITRAMFS_PROFILE ?= normal
ifeq ($(INITRAMFS_PROFILE),regression)
  KERNEL_INITRAMFS_CPIO_LA := $(REGRESSION_CPIO_LA)
else
  KERNEL_INITRAMFS_CPIO_LA := $(INITRAMFS_CPIO_LA)
endif
# Cargo has no initramfs-profile feature: profile selection is solely the
# MANGO_INITRAMFS_CPIO environment value passed by the architecture Makefile.

KERNEL_CMDLINE ?= mango.mode=normal

# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_LA_OUTPUT_DIR))
LWEXT4_LA_PREREQ := lwext4-la64

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
# Builds minimal initramfs with /init=regression_init and
# /regression. Launches QEMU with NO disk drives. Parses
# console for [L4 REGRESSION RESULT: PASS] / FAIL markers.
REGRESSION_CMDLINE := mango.mode=regression
