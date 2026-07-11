# Building
TARGET := riscv64gc-unknown-none-elf
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
DISASM_TMP := target/$(TARGET)/$(MODE)/asm
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

lwext4-rv64: $(LWEXT4_RV_LIB)

$(LWEXT4_RV_LIB):
	@echo "=== Building lwext4 C library for riscv64 ==="
	@# Copy our cmake toolchain (linux-gnu) over the musl-generic one
	@cp -f $(LWEXT4_CMAKE) $(LWEXT4_DIR)/toolchain/musl-generic.cmake
	@# Copy ulibc.c into lwext4 src tree so it gets compiled into the .a
	@cp -f ../dependency/lwext4_rust/c/ulibc.c $(LWEXT4_DIR)/src/ulibc.c
	@# Ensure ulibc.c is in the library sources (no git apply needed)
	@grep -q 'ulibc.c' $(LWEXT4_DIR)/src/CMakeLists.txt 2>/dev/null || \
		sed -i '/aux_source_directory/a set(M_SRC ulibc.c)' $(LWEXT4_DIR)/src/CMakeLists.txt
	@grep -q '$${M_SRC}' $(LWEXT4_DIR)/src/CMakeLists.txt 2>/dev/null || \
		sed -i 's/add_library(lwext4 STATIC $${LWEXT4_SRC})/add_library(lwext4 STATIC $${LWEXT4_SRC} $${M_SRC})/' $(LWEXT4_DIR)/src/CMakeLists.txt
	@# Build with cmake directly (bypasses the lwext4 Makefile)
	@mkdir -p $(LWEXT4_DIR)/build_lwext4-rv64
	@cd $(LWEXT4_DIR)/build_lwext4-rv64 && \
		ARCH=riscv64 cmake -G"Unix Makefiles" \
			-DCMAKE_BUILD_TYPE=Release \
			-DVERSION_MAJOR=1 -DVERSION_MINOR=0 -DVERSION_PATCH=0 \
			-DLWEXT4_BUILD_SHARED_LIB=OFF \
			-DLIB_ONLY=TRUE \
			-DCMAKE_TOOLCHAIN_FILE=../toolchain/musl-generic.cmake \
			.. 2>&1 | tail -5
	@cd $(LWEXT4_DIR)/build_lwext4-rv64 && make lwext4 -j$$(nproc)
	@cp -f $(LWEXT4_DIR)/build_lwext4-rv64/src/liblwext4.a $(LWEXT4_RV_LIB)
	@echo "=== lwext4 riscv64 .a built at $(LWEXT4_RV_LIB) ==="

clean-lwext4-rv:
	@rm -rf $(LWEXT4_DIR)/build_lwext4-rv64 $(LWEXT4_RV_LIB)

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
HOST_TRIPLE := $(shell rustc -vV | sed -n 's/^host: //p')
LLVM_TOOLS_DIR := $(shell rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin
OBJDUMP := $(LLVM_TOOLS_DIR)/rust-objdump --arch-name=riscv64
OBJCOPY := $(LLVM_TOOLS_DIR)/rust-objcopy --binary-architecture=riscv64

# Disassembly
DISASM ?= -x

all: fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) ../kernel-rv

mv-debug:
	cp -f $(KERNEL_ELF) ../kernel-rv

build: env $(KERNEL_BIN) mv

env:
	(rustup target list | grep "riscv64gc-unknown-none-elf (installed)") || rustup target add $(TARGET)
	rustup target add $(TARGET)
	rustup component add rust-src
	rustup component add llvm-tools-preview

# build all user programs
user:
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: user
	./buildfs.sh "$(ROOTFS_IMG)" "rvqemu" $(MODE) $(FS_MODE)

# Initramfs cpio generation (always needed when feature is in Cargo defaults)
INITRAMFS_CPIO_RV := ../fs-img-dir/initramfs-rv.cpio

kernel: $(INITRAMFS_CPIO_RV)

$(INITRAMFS_CPIO_RV): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh rv64 $(MODE) $(INITRAMFS_CPIO_RV)
	@touch src/initramfs-rv.S  # 强制 Cargo 重编译（.incbin 时间戳变化）

# xein TODO: 注意需要评估zero_init启用与否的影响
# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_DIR))
LWEXT4_PREREQ := lwext4-rv64

kernel: $(LWEXT4_PREREQ)
	@echo Platform: $(BOARD)
	@cp -f src/hal/arch/riscv/linker-$(BOARD).ld src/hal/arch/riscv/linker.ld
    ifeq ($(MODE), debug)
		@LOG=${LOG} cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    else
		@LOG=${LOG} cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    endif

clean:
	@which cargo >/dev/null 2>&1 && cargo clean || true
	@rm -rf $(KERNEL_RV)
	@rm -rf $(LWEXT4_DIR)/build_lwext4-rv64 $(LWEXT4_RV_LIB)

run: build
ifeq ($(BOARD), rvqemu)
	@qemu-system-riscv64 \
  		-machine virt \
  		-nographic \
  		-bios $(BOOTLOADER) \
  		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
        -drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
        $(BLK_DEV_x0) \
        -drive if=none,file=../disk.img,format=raw,id=x1 \
        $(BLK_DEV_x1) \
  		-m 1024 \
  		-smp threads=$(CORE_NUM)
endif

monitor:
	riscv64-unknown-elf-gdb -ex 'file target/riscv64gc-unknown-none-elf/debug/os' -ex 'set arch riscv:rv64' -ex 'target remote localhost:1234'

gdb:
	@qemu-system-riscv64 \
	-machine virt \
	-nographic \
	-bios $(BOOTLOADER) \
	-device loader,file=target/riscv64gc-unknown-none-elf/debug/os,addr=0x80200000 \
	-drive file=$(ROOTFS_IMG),if=none,format=raw,id=x0 \
	$(BLK_DEV_x0) \
	-drive file=../disk.img,if=none,format=raw,id=x1 \
	$(BLK_DEV_x1) \
	-m 1024 \
	-smp threads=$(CORE_NUM) -S -s | tee qemu.log

runsimple:
	@qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-drive file=$(ROOTFS_IMG),if=none,format=raw,id=x0 \
		-m 1024 \
        $(BLK_DEV_x0) \
		-drive file=../disk.img,if=none,format=raw,id=x1 \
        $(BLK_DEV_x1) \
		-smp threads=$(CORE_NUM)

comp:
	@qemu-system-riscv64 \
		-machine virt \
		-kernel $(KERNEL_RV) \
		-m 1024 \
		-nographic \
		-smp 1 \
		-bios default \
		-drive file=$(SDCARD_RV),if=none,format=raw,id=x0 \
		$(BLK_DEV_x0) \
		-drive file=../disk.img,if=none,format=raw,id=x1 \
		$(BLK_DEV_x1) \
		-no-reboot \
		-rtc base=utc \
		$(NET_DEV) \
		-object filter-dump,id=f1,netdev=net,file=packets.pcap

comp-gdb:
	@qemu-system-riscv64 \
        -machine virt \
        -kernel $(KERNEL_RV) \
        -m 1024 \
        -nographic \
        -smp 1 \
        -bios default \
        -drive file=$(SDCARD_RV),if=none,format=raw,id=x0 \
        $(BLK_DEV_x0) \
        -drive file=../disk.img,if=none,format=raw,id=x1 \
        $(BLK_DEV_x1) \
        -no-reboot \
        -rtc base=utc \
	$(NET_DEV) \
	-object filter-dump,id=f1,netdev=net,file=packets.pcap \
        -S \
        -s

.PHONY: user

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
# The kernel needs initramfs cpio (embedded via .S), so user
# programs must be built first.
ktest-run: user $(LWEXT4_PREREQ)
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@cp -f src/hal/arch/riscv/linker-$(BOARD).ld src/hal/arch/riscv/linker.ld
	@MANGO_CMDLINE="$(KTEST_CMDLINE)" LOG=${LOG} \
		cargo build --$(MODE) --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)
	@echo "[ktest] Launching QEMU (timeout: ${KTEST_QEMU_TIMEOUT}s)..."
	@timeout --foreground ${KTEST_QEMU_TIMEOUT} qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-m 512 \
		-smp threads=1

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
# Builds minimal initramfs with /init=regression_init and
# /regression. Launches QEMU with NO disk drives. Parses
# console for [L4 REGRESSION RESULT: PASS] / FAIL markers.
REGRESSION_CMDLINE := mango.mode=regression

regression-run: user $(LWEXT4_PREREQ)
	@echo "[regression] Building regression initramfs..."
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh rv64 $(MODE) $(INITRAMFS_CPIO_RV) regression
	@echo "[regression] Rebuilding kernel with: $(REGRESSION_CMDLINE)"
	@cp -f src/hal/arch/riscv/linker-$(BOARD).ld src/hal/arch/riscv/linker.ld
	@MANGO_CMDLINE="$(REGRESSION_CMDLINE)" LOG=${LOG} \
		cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)
	@echo "[regression] Launching QEMU (no disks, timeout 60s)..."
	@timeout --foreground 60 qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-m 256 \
		-smp threads=1 2>&1 | tee /tmp/regression-rv.log
	@grep -q "L4 REGRESSION RESULT: PASS" /tmp/regression-rv.log \
		&& echo "=== REGRESSION PASS ===" \
		|| (echo "=== REGRESSION FAIL ===" && exit 1)
