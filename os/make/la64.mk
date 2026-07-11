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

lwext4-la64: $(LWEXT4_LA_LIB)

$(LWEXT4_LA_LIB):
	@echo "=== Building lwext4 C library for loongarch64 ==="
	@cp -f ../dependency/lwext4_rust/c/elf-linux-gnu.cmake $(LWEXT4_LA_DIR)/toolchain/musl-generic.cmake
	@cp -f ../dependency/lwext4_rust/c/ulibc.c $(LWEXT4_LA_DIR)/src/ulibc.c
	@grep -q 'ulibc.c' $(LWEXT4_LA_DIR)/src/CMakeLists.txt 2>/dev/null || \
		sed -i '/aux_source_directory/a set(M_SRC ulibc.c)' $(LWEXT4_LA_DIR)/src/CMakeLists.txt
	@grep -q '$${M_SRC}' $(LWEXT4_LA_DIR)/src/CMakeLists.txt 2>/dev/null || \
		sed -i 's/add_library(lwext4 STATIC $${LWEXT4_SRC})/add_library(lwext4 STATIC $${LWEXT4_SRC} $${M_SRC})/' $(LWEXT4_LA_DIR)/src/CMakeLists.txt
	@mkdir -p $(LWEXT4_LA_DIR)/build_lwext4-la64
	@PATH="$(LWEXT4_LA_TOOLCHAIN_PATH):$$PATH" \
	 ARCH=loongarch64 cmake -G"Unix Makefiles" \
	   -DCMAKE_BUILD_TYPE=Release \
	   -DVERSION_MAJOR=1 -DVERSION_MINOR=0 -DVERSION_PATCH=0 \
	   -DLWEXT4_BUILD_SHARED_LIB=OFF \
	   -DLIB_ONLY=TRUE \
	   -DCMAKE_TOOLCHAIN_FILE=../toolchain/musl-generic.cmake \
	   -S $(LWEXT4_LA_DIR) \
	   -B $(LWEXT4_LA_DIR)/build_lwext4-la64 2>&1 | tail -5
	@PATH="$(LWEXT4_LA_TOOLCHAIN_PATH):$$PATH" \
	 $(MAKE) -C $(LWEXT4_LA_DIR)/build_lwext4-la64 lwext4 -j$$(nproc)
	@cp -f $(LWEXT4_LA_DIR)/build_lwext4-la64/src/liblwext4.a $(LWEXT4_LA_LIB)
	@echo "=== lwext4 loongarch64 .a built ==="

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

# ============================================================
# Targets (symmetric with rv64.mk)
# ============================================================

all: fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

mv-debug:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

build: env $(KERNEL_BIN) mv

env:
	(rustup target list | grep "$(TARGET) (installed)") || rustup target add $(TARGET)
	rustup component add rust-src

# Build all user programs
user:
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: user
	./buildfs.sh "$(ROOTFS_IMG)" "$(BOARD)" $(MODE) $(FS_MODE)

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

kernel: $(KERNEL_INITRAMFS_CPIO_LA)

$(INITRAMFS_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)
	@touch src/initramfs-la.S

$(REGRESSION_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh la64 $(MODE) $(REGRESSION_CPIO_LA) regression
	@touch src/initramfs-regression-la.S

# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_LA_DIR))
LWEXT4_LA_PREREQ := lwext4-la64

kernel: $(LWEXT4_LA_PREREQ)
	@echo Platform: $(BOARD)
	@cp -f src/hal/arch/loongarch64/linker-$(BOARD).ld src/hal/arch/loongarch64/linker.ld 2>/dev/null || true 2>/dev/null || true
ifeq ($(MODE), debug)
	@MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=$(LOG) cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
else
	@MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=$(LOG) cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
endif

# uImage (la64-specific: for uboot boot)
uimage: $(KERNEL_BIN)
	../util/mkimage -A loongarch -O linux -T kernel -C none \
	  -a $(LA_LOAD_ADDR) -e $(LA_ENTRY_POINT) \
	  -n NPUcore+ -d $(KERNEL_BIN) $(KERNEL_UIMG)

clean:
	@which cargo >/dev/null 2>&1 && cargo clean || true
	@rm -rf $(KERNEL_LA)
	@rm -rf $(LWEXT4_LA_DIR)/build_lwext4-la64 $(LWEXT4_LA_LIB)

# ============================================================
# QEMU run targets
# ============================================================

run: build
ifeq ($(BOARD), laqemu)
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(DISK_LA),format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-m 1024 \
		-smp threads=$(CORE_NUM)
endif

runsimple:
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(DISK_LA),format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-m 1024 \
		-smp threads=$(CORE_NUM)

comp:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel $(KERNEL_LA) \
		-m 1G \
		-nographic \
		-smp 1 \
		-drive file=$(SDCARD_LA),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive file=$(DISK_LA),if=none,format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

comp-gdb:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel $(KERNEL_LA) \
		-m 1024 \
		-nographic \
		-smp 1 \
		-drive file=$(SDCARD_LA),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive file=$(DISK_LA),if=none,format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-no-reboot \
		-rtc base=utc \
		-S \
		-s

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
ktest-run: user $(LWEXT4_LA_PREREQ)
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@cp -f src/hal/arch/loongarch64/linker-$(BOARD).ld src/hal/arch/loongarch64/linker.ld 2>/dev/null || true
	@MANGO_CMDLINE="$(KTEST_CMDLINE)" LOG=${LOG} \
		cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)
	@echo "[ktest] Launching QEMU (timeout: ${KTEST_QEMU_TIMEOUT}s)..."
	@timeout --foreground ${KTEST_QEMU_TIMEOUT} qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-m 1024 \
		-smp threads=1

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
# Builds minimal initramfs with /init=regression_init and
# /regression. Launches QEMU with NO disk drives. Parses
# console for [L4 REGRESSION RESULT: PASS] / FAIL markers.
REGRESSION_CMDLINE := mango.mode=regression

regression-run:
	@echo "[regression] Building la64 kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU (no disks, timeout 60s)..."
	@timeout --foreground 60 qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-m 1024 \
		-smp threads=1 2>&1 | tee /tmp/regression-la.log
	@grep -q "L4 REGRESSION RESULT: PASS" /tmp/regression-la.log \
		&& echo "=== REGRESSION PASS ===" \
		|| (echo "=== REGRESSION FAIL ===" && exit 1)
