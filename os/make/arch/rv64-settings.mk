# Building
TARGET := riscv64gc-unknown-none-elf
MODE := release
KERNEL_OUTPUT_ROOT ?= target
KERNEL_ELF := $(KERNEL_OUTPUT_ROOT)/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
DISASM_TMP := $(KERNEL_OUTPUT_ROOT)/$(TARGET)/$(MODE)/asm
BLK_MODE ?= virt
# QEMU device types based on transport
ifeq ($(BLK_MODE),virt_pci)
  BLK_DEV_x0 = -device virtio-blk-pci,drive=x0
  BLK_DEV_x1 = -device virtio-blk-pci,drive=x1
  NET_DEV     = -device virtio-net-pci,netdev=net -netdev user,id=net
else
  BLK_DEV_x0 = -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0
  BLK_DEV_x1 = -device virtio-blk-device,drive=x1,bus=virtio-mmio-bus.1
  NET_DEV     = -device virtio-net-device,netdev=net,bus=virtio-mmio-bus.7 -netdev user,id=net
endif
FS_MODE ?= ext4
ROOTFS_IMG_NAME = rootfs-rv.img
ROOTFS_IMG_DIR := ../fs-img-dir
CORE_NUM := 1
LOG ?= off
KERNEL_RV := ../kernel-rv
KERNEL_LA := ../kernel-la
SDCARD_RV := ../sdcard-rv.img
SDCARD_LA := ../sdcard-la.img

# ============================================================
# lwext4 C library
# ============================================================
LWEXT4_DIR := ../dependency/lwext4_rust/c/lwext4
LWEXT4_RV_LIB := $(LWEXT4_DIR)/liblwext4-riscv64.a
LWEXT4_CMAKE := ../dependency/lwext4_rust/c/elf-linux-gnu.cmake
LWEXT4_PATCH := ../dependency/lwext4_rust/c/lwext4-make.patch
# Prerequisites for archive rebuild invalidation
LWEXT4_RV_SRCS := $(wildcard $(LWEXT4_DIR)/src/*.c)
LWEXT4_RV_HDRS := $(wildcard $(LWEXT4_DIR)/include/*.h) $(wildcard $(LWEXT4_DIR)/include/misc/*.h)
LWEXT4_RV_CMAKE_INPUTS := $(LWEXT4_DIR)/CMakeLists.txt \
                          $(LWEXT4_DIR)/src/CMakeLists.txt \
                          $(LWEXT4_CMAKE) \
                          ../dependency/lwext4_rust/c/ulibc.c

ifeq ($(BOARD), vf2)
ROOTFS_IMG := /dev/sdc
else
ROOTFS_IMG := ${ROOTFS_IMG_DIR}/${ROOTFS_IMG_NAME}
endif

APPS := ../user/src/bin/*

# BOARD
BOARD ?= rvqemu
# xein TODO: 下面代码因sbi版本改变确定无用后需要进行缩减
SBI ?= opensbi-1.0
ifeq ($(BOARD), rvqemu)
	ifeq ($(SBI), rustsbi)
		BOOTLOADER := ../bootloader/$(SBI)-$(BOARD).bin
	else ifeq ($(SBI), default)
		BOOTLOADER := default
	else
		BOOTLOADER := ../bootloader/fw_payload.bin
	endif
else ifeq ($(BOARD), vf2)
	BOOTLOADER := ../bootloader/rustsbi-$(BOARD).bin
endif

ifndef LOG
	LOG_OPTION := "log_off"
else
	LOG_OPTION := "log_${LOG}"
endif

# KERNEL ENTRY
ifeq ($(BOARD), rvqemu)
	KERNEL_ENTRY_PA := 0x80200000
else ifeq ($(BOARD), vf2)
	KERNEL_ENTRY_PA := 0x40200000
endif

# Binutils from rustup's llvm-tools-preview component. This avoids depending on
# the cargo-binutils wrapper being preinstalled or downloaded during grading.
HOST_TRIPLE := $(shell env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 rustc -vV | sed -n 's/^host: //p')
LLVM_TOOLS_DIR := $(shell env -u RUSTUP_TOOLCHAIN RUSTUP_AUTO_INSTALL=0 rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin
OBJDUMP := $(LLVM_TOOLS_DIR)/rust-objdump --arch-name=riscv64
OBJCOPY := $(LLVM_TOOLS_DIR)/rust-objcopy --binary-architecture=riscv64

# Disassembly
DISASM ?= -x

# Initramfs cpio generation — parameterized for normal / regression profiles
INITRAMFS_CPIO_RV := ../fs-img-dir/initramfs-rv.cpio
REGRESSION_CPIO_RV := ../fs-img-dir/initramfs-regression-rv.cpio

# INITRAMFS_PROFILE: "normal" (default) or "regression"
INITRAMFS_PROFILE ?= normal

ifeq ($(INITRAMFS_PROFILE),regression)
  KERNEL_INITRAMFS_CPIO_RV := $(REGRESSION_CPIO_RV)
  INITRAMFS_PROFILE_FEATURES := regression_initramfs
else
  KERNEL_INITRAMFS_CPIO_RV := $(INITRAMFS_CPIO_RV)
  INITRAMFS_PROFILE_FEATURES :=
endif

KERNEL_CMDLINE ?= mango.mode=normal

# xein TODO: 注意需要评估zero_init启用与否的影响
# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_DIR))
LWEXT4_PREREQ := lwext4-rv64

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
REGRESSION_CMDLINE := mango.mode=regression
