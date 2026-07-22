include make/common/toolchain.mk
include make/image-roles.mk
include make/arch/la64-settings.mk

lwext4-la64: $(LWEXT4_LA_LIB)

$(LWEXT4_LA_PREPARED): $(LWEXT4_LA_INPUTS)
	@rm -rf $(LWEXT4_LA_SOURCE_DIR) $(LWEXT4_LA_BUILD_DIR)
	@mkdir -p $(LWEXT4_LA_SOURCE_DIR)
	@tar -C $(LWEXT4_LA_DIR) --exclude='build_*' -cf - . | tar -C $(LWEXT4_LA_SOURCE_DIR) -xf -
	@cp -f ../dependency/lwext4_rust/c/ulibc.c $(LWEXT4_LA_SOURCE_DIR)/src/ulibc.c
	@touch $@

$(LWEXT4_LA_LIB): $(LWEXT4_LA_PREPARED)
	@echo "=== Building lwext4 C library for loongarch64 ==="
	@PATH="$(LWEXT4_LA_TOOLCHAIN_PATH):$$PATH" \
	 ARCH=loongarch64 cmake -G"Unix Makefiles" \
	   -DCMAKE_BUILD_TYPE=Release \
	   -DVERSION_MAJOR=1 -DVERSION_MINOR=0 -DVERSION_PATCH=0 \
	   -DLWEXT4_BUILD_SHARED_LIB=OFF \
	   -DLIB_ONLY=TRUE \
	   -DCMAKE_TOOLCHAIN_FILE=$(abspath $(LWEXT4_LA_CMAKE)) \
	   -S $(LWEXT4_LA_SOURCE_DIR) \
	   -B $(LWEXT4_LA_BUILD_DIR)
	@PATH="$(LWEXT4_LA_TOOLCHAIN_PATH):$$PATH" \
	 cmake --build $(LWEXT4_LA_BUILD_DIR) --target lwext4 --parallel $$(nproc)
	@cp -f $(LWEXT4_LA_BUILD_DIR)/src/liblwext4.a $(LWEXT4_LA_LIB)
	@echo "=== lwext4 loongarch64 .a built ==="

# ============================================================
# Targets (symmetric with rv64.mk)
# ============================================================

all: toolchain-preflight fs-img build

debug: build mv-debug

stage-kernel:
	@mkdir -p $(dir $(KERNEL_LA))
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

mv: stage-kernel
	@echo "[deprecated] mv stages the LA64 kernel; root publication happens only after make all succeeds"

mv-debug:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

build: env $(KERNEL_BIN) stage-kernel

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight

# Build all user programs
user: toolchain-preflight
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE) USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)"

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: toolchain-preflight user
	@mkdir -p $(dir $(ROOTFS_IMG))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ./buildfs.sh "$(ROOTFS_IMG)" "$(BOARD)" $(MODE) $(FS_MODE)

kernel: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_LA)

$(INITRAMFS_CPIO_LA): user
	@mkdir -p $(dir $(INITRAMFS_CPIO_LA))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ./build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)

$(REGRESSION_CPIO_LA): user
	@mkdir -p $(dir $(REGRESSION_CPIO_LA))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ./build_initramfs.sh la64 $(MODE) $(REGRESSION_CPIO_LA) regression

kernel: $(LWEXT4_LA_PREREQ)
	@echo Platform: $(BOARD)
ifeq ($(MODE), debug)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
else
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
endif

# uImage (la64-specific: for uboot boot)
uimage: $(KERNEL_BIN)
	../util/mkimage -A loongarch -O linux -T kernel -C none \
	  -a $(LA_LOAD_ADDR) -e $(LA_ENTRY_POINT) \
	  -n NPUcore+ -d $(KERNEL_BIN) $(KERNEL_UIMG)

clean:
	@rm -rf "$(KERNEL_OUTPUT_ROOT)" "$(LWEXT4_LA_OUTPUT_DIR)"

# ============================================================
# QEMU run targets
# ============================================================

run: toolchain-preflight build
ifeq ($(BOARD), laqemu)
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(IMAGE_ROLE_LA64_DEVELOPMENT_X0),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(IMAGE_ROLE_LA64_X1),format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-m 1024 \
		-smp threads=$(CORE_NUM)
endif

runsimple: toolchain-preflight
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(IMAGE_ROLE_LA64_DEVELOPMENT_X0),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(IMAGE_ROLE_LA64_X1),format=raw,id=x1 \
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
		-drive file=$(IMAGE_ROLE_LA64_COMPETITION_X0),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive file=$(IMAGE_ROLE_LA64_X1),if=none,format=raw,id=x1 \
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
		-drive file=$(IMAGE_ROLE_LA64_COMPETITION_X0),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive file=$(IMAGE_ROLE_LA64_X1),if=none,format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		-no-reboot \
		-rtc base=utc \
		-S \
		-s

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb env toolchain-preflight check ktest-build-only

check: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_LA)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) \
		cargo check --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
ktest-build-only: toolchain-preflight user $(KERNEL_INITRAMFS_CPIO_LA) $(LWEXT4_LA_PREREQ)
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KTEST_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} \
		cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)

ktest-run: toolchain-preflight ktest-build-only
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
