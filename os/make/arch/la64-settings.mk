# Building
TARGET := loongarch64-unknown-linux-gnu
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
KERNEL_UIMG := $(KERNEL_ELF).ui
BLK_MODE := virt_pci
FS_MODE ?= ext4
ROOTFS_IMG_NAME = rootfs-la.img
ROOTFS_IMG_DIR := ../fs-img-dir
CORE_NUM := 1
LOG ?= off
KERNEL_LA := ../kernel-la
SDCARD_LA := ../sdcard-la.img
DISK_LA := ../disk-la.img

# ============================================================
# lwext4 C library (la64: uses pre-installed cross-compiler)
# ============================================================
LWEXT4_LA_DIR := ../dependency/lwext4_rust/c/lwext4
LWEXT4_LA_LIB := $(LWEXT4_LA_DIR)/liblwext4-loongarch64.a
LWEXT4_LA_TOOLCHAIN_PATH := /opt/gcc-13.2.0-loongarch64-linux-gnu/bin
# Prerequisites for archive rebuild invalidation
LWEXT4_LA_SRCS := $(wildcard $(LWEXT4_LA_DIR)/src/*.c)
LWEXT4_LA_HDRS := $(wildcard $(LWEXT4_LA_DIR)/include/*.h) $(wildcard $(LWEXT4_LA_DIR)/include/misc/*.h)
LWEXT4_LA_CMAKE_INPUTS := $(LWEXT4_LA_DIR)/CMakeLists.txt \
                          $(LWEXT4_LA_DIR)/src/CMakeLists.txt \
                          ../dependency/lwext4_rust/c/elf-linux-gnu.cmake \
                          ../dependency/lwext4_rust/c/ulibc.c

# BOARD
BOARD ?= laqemu

# Logging
ifndef LOG
	LOG_OPTION := "log_off"
else
	LOG_OPTION := "log_${LOG}"
endif

# Kernel entry (for -device loader fallback)
KERNEL_ENTRY_PA := 0x9000000090000000

# Binutils (cross toolchain, not llvm)
OBJCOPY := loongarch64-linux-gnu-objcopy
OBJDUMP := loongarch64-linux-gnu-objdump
READELF := loongarch64-linux-gnu-readelf

# uImage config
LA_LOAD_ADDR := 0x9000000090000000
LA_ENTRY_POINT := 0x9000000090000000

# Applications
APPS := ../user/src/bin/*

# RootFS image
ifeq ($(BOARD), laqemu)
	ROOTFS_IMG := ${ROOTFS_IMG_DIR}/${ROOTFS_IMG_NAME}
endif

# Initramfs cpio generation — parameterized for normal / regression profiles
INITRAMFS_CPIO_LA := ../fs-img-dir/initramfs-la.cpio
REGRESSION_CPIO_LA := ../fs-img-dir/initramfs-regression-la.cpio

INITRAMFS_PROFILE ?= normal
ifeq ($(INITRAMFS_PROFILE),regression)
  KERNEL_INITRAMFS_CPIO_LA := $(REGRESSION_CPIO_LA)
  INITRAMFS_PROFILE_FEATURES := regression_initramfs
else
  KERNEL_INITRAMFS_CPIO_LA := $(INITRAMFS_CPIO_LA)
  INITRAMFS_PROFILE_FEATURES :=
endif

KERNEL_CMDLINE ?= mango.mode=normal

# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_LA_DIR))
LWEXT4_LA_PREREQ := lwext4-la64

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
# Builds minimal initramfs with /init=regression_init and
# /regression. Launches QEMU with NO disk drives. Parses
# console for [L4 REGRESSION RESULT: PASS] / FAIL markers.
REGRESSION_CMDLINE := mango.mode=regression
