include make/common/toolchain.mk
include make/arch/rv64-settings.mk

lwext4-rv64: $(LWEXT4_RV_LIB)

$(LWEXT4_RV_LIB): $(LWEXT4_RV_SRCS) $(LWEXT4_RV_HDRS) $(LWEXT4_RV_CMAKE_INPUTS)
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

all: toolchain-preflight fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) ../kernel-rv

mv-debug:
	cp -f $(KERNEL_ELF) ../kernel-rv

build: env $(KERNEL_BIN) mv

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight

# build all user programs
user: toolchain-preflight
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: toolchain-preflight user
	./buildfs.sh "$(ROOTFS_IMG)" "rvqemu" $(MODE) $(FS_MODE)

kernel: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_RV)

$(INITRAMFS_CPIO_RV): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh rv64 $(MODE) $(INITRAMFS_CPIO_RV)

$(REGRESSION_CPIO_RV): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh rv64 $(MODE) $(REGRESSION_CPIO_RV) regression
	@touch src/initramfs-regression-rv.S

kernel: $(LWEXT4_PREREQ)
	@echo Platform: $(BOARD)
    ifeq ($(MODE), debug)
		@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=${LOG} cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)"
    else
		@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=${LOG} cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)"
    endif

clean:
	@which cargo >/dev/null 2>&1 && cargo clean || true
	@rm -rf $(KERNEL_RV)
	@rm -rf $(LWEXT4_DIR)/build_lwext4-rv64 $(LWEXT4_RV_LIB)

run: toolchain-preflight build
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

runsimple: toolchain-preflight
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

comp: toolchain-preflight
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

comp-gdb: toolchain-preflight
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

.PHONY: user env toolchain-preflight

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
# The kernel needs initramfs cpio (embedded via .S), so user
# programs must be built first.
ktest-run: toolchain-preflight user $(LWEXT4_PREREQ)
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
		-m 1024 \
		-smp threads=1

regression-run: toolchain-preflight
	@echo "[regression] Building kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU (no disks, timeout 60s)..."
	@timeout --foreground 60 qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-m 1024 \
		-smp threads=1 2>&1 | tee /tmp/regression-rv.log
	@grep -q "L4 REGRESSION RESULT: PASS" /tmp/regression-rv.log \
		&& echo "=== REGRESSION PASS ===" \
		|| (echo "=== REGRESSION FAIL ===" && exit 1)
