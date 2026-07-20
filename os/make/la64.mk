include make/common/toolchain.mk
include make/arch/la64-settings.mk

lwext4-la64: $(LWEXT4_LA_LIB)

$(LWEXT4_LA_LIB): $(LWEXT4_LA_SRCS) $(LWEXT4_LA_HDRS) $(LWEXT4_LA_CMAKE_INPUTS)
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

# ============================================================
# Targets (symmetric with rv64.mk)
# ============================================================

all: toolchain-preflight fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

mv-debug:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

build: env $(KERNEL_BIN) mv

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight

# Build all user programs
user: toolchain-preflight
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: toolchain-preflight user
	./buildfs.sh "$(ROOTFS_IMG)" "$(BOARD)" $(MODE) $(FS_MODE)

kernel: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_LA)

$(INITRAMFS_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)

$(REGRESSION_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh la64 $(MODE) $(REGRESSION_CPIO_LA) regression
	@touch src/initramfs-regression-la.S

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

run: toolchain-preflight build
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

runsimple: toolchain-preflight
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

comp: toolchain-preflight
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

comp-gdb: toolchain-preflight
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

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb env toolchain-preflight

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
ktest-run: toolchain-preflight user $(LWEXT4_LA_PREREQ)
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

regression-run: toolchain-preflight
	@echo "[regression] Building la64 kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU (no disks, timeout 60s)..."
	@timeout --foreground 60 qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-m 1024 \
		-smp threads=1 >/tmp/regression-la.log 2>&1; \
	qemu_status=$$?; \
	cat /tmp/regression-la.log; \
	state=$$(../scripts/check-la64-regression-log.sh /tmp/regression-la.log $$qemu_status); \
	printf '%s\n' "$$state"; \
	case "$$state" in \
		"STATE=PASS STATUS=0") echo "=== REGRESSION PASS ==="; exit 0 ;; \
		"STATE=BLOCKED_STAGE1_PRE_ENTRY STATUS="*|"STATE=BLOCKED_STAGE1_POST_ENTRY STATUS="*) echo "=== REGRESSION BLOCKED ==="; exit 1 ;; \
		*) echo "=== REGRESSION FAIL ==="; exit 1 ;; \
	esac
