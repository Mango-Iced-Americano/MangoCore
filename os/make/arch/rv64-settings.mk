# Building
BUILD_ROOT ?= $(abspath ../build)
PROFILE ?= normal
TARGET := riscv64gc-unknown-none-elf
MODE := release
PRODUCT_ROOT ?= $(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)
KERNEL_OUTPUT_ROOT ?= $(PRODUCT_ROOT)/kernel
USER_OUTPUT_ROOT ?= $(PRODUCT_ROOT)/user
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
IMAGE_ROLE_RV64_PRODUCT_ROOT := $(PRODUCT_ROOT)
# 同一个值同时约束 Cargo 中编译期拓扑与所有 QEMU profile，避免内核和
# 虚拟机看到不同的 CPU 数量。
CORE_NUM ?= 1
VALID_CORE_NUMS := 1 2 4 8
# 用立即展开的声明完成校验，既在解析期 fail-closed，也保持 settings 模块
# 只有声明、没有 target/recipe 的分层约束。
CORE_NUM_VALIDATION := $(if $(filter $(CORE_NUM),$(VALID_CORE_NUMS)),,$(error CORE_NUM must be one of $(VALID_CORE_NUMS), got '$(CORE_NUM)'))
export MANGO_CORE_NUM := $(CORE_NUM)
LOG ?= off
KERNEL_RV := $(PRODUCT_ROOT)/kernel/kernel-rv
KERNEL_LA := $(PRODUCT_ROOT)/kernel/kernel-la
SDCARD_RV := $(IMAGE_ROLE_RV64_COMPETITION_X0)

# ============================================================
# lwext4 C library
# ============================================================
LWEXT4_DIR := ../dependency/lwext4_rust/c/lwext4
LWEXT4_RV_OUTPUT_DIR := $(KERNEL_OUTPUT_ROOT)/lwext4/rv64
LWEXT4_RV_SOURCE_DIR := $(LWEXT4_RV_OUTPUT_DIR)/source
LWEXT4_RV_BUILD_DIR := $(LWEXT4_RV_OUTPUT_DIR)/build
LWEXT4_RV_LIB := $(LWEXT4_RV_OUTPUT_DIR)/liblwext4-riscv64.a
LWEXT4_RV_PREPARED := $(LWEXT4_RV_OUTPUT_DIR)/.prepared
LWEXT4_CMAKE := ../dependency/lwext4_rust/c/elf-linux-gnu.cmake
# Prerequisites for archive rebuild invalidation
LWEXT4_RV_INPUTS := $(shell find "$(LWEXT4_DIR)" -type f ! -path "$(LWEXT4_DIR)/build_*/*") \
                    $(LWEXT4_CMAKE) \
                    ../dependency/lwext4_rust/c/ulibc.c

ifeq ($(BOARD), vf2)
ROOTFS_IMG := /dev/sdc
else
ROOTFS_IMG := $(IMAGE_ROLE_RV64_DEVELOPMENT_X0)
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
INITRAMFS_CPIO_RV := $(IMAGE_ROLE_RV64_BOOTSTRAP_ROOT)
REGRESSION_CPIO_RV := $(PRODUCT_ROOT)/initramfs/initramfs-regression-rv.cpio

# INITRAMFS_PROFILE: "normal" (default) or "regression"
INITRAMFS_PROFILE ?= normal

ifeq ($(INITRAMFS_PROFILE),regression)
  KERNEL_INITRAMFS_CPIO_RV := $(REGRESSION_CPIO_RV)
else
  KERNEL_INITRAMFS_CPIO_RV := $(INITRAMFS_CPIO_RV)
endif
# Cargo has no initramfs-profile feature: profile selection is solely the
# MANGO_INITRAMFS_CPIO environment value passed by the architecture Makefile.

KERNEL_CMDLINE ?= mango.mode=normal

# xein TODO: 注意需要评估zero_init启用与否的影响
# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_RV_OUTPUT_DIR))
LWEXT4_PREREQ := lwext4-rv64

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
REGRESSION_CMDLINE := mango.mode=regression
