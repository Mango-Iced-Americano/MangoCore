include make/common/toolchain.mk
include make/image-roles.mk
include make/arch/la64-settings.mk
include make/common/orchestration.mk
include make/qemu-profiles.mk

QEMU_EXECUTABLE = qemu-system-loongarch64
QEMU_ROLE_ARCH = LA64
QEMU_COMPETITION_X0 = $(IMAGE_ROLE_LA64_COMPETITION_X0)
QEMU_DERIVED_X0 = $(IMAGE_ROLE_LA64_DERIVED_X0)
QEMU_DEVELOPMENT_X0 = $(IMAGE_ROLE_LA64_DEVELOPMENT_X0)
QEMU_BUILDSTORM_X0 = $(IMAGE_ROLE_LA64_BUILDSTORM_X0)
BUILDSTORM_GOLDEN_X0 = $(IMAGE_ROLE_LA64_BUILDSTORM_GOLDEN_X0)
BUILDSTORM_ARCHIVE = $(IMAGE_ROLE_LA64_BUILDSTORM_ARCHIVE)
QEMU_COMPETITION_BEFORE_DRIVES = -kernel $(KERNEL_LA) -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_COMPETITION_AFTER_DRIVES = -no-reboot $(NET_DEV) -rtc base=utc
QEMU_COMPETITION_GDB_BEFORE_DRIVES = -kernel $(KERNEL_LA) -m $(QEMU_MEMORY) -smp 1
QEMU_COMPETITION_GDB_AFTER_DRIVES = -no-reboot -rtc base=utc -S -s
QEMU_BUILDSTORM_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(BUILDSTORM_KERNEL_LA) -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_BUILDSTORM_AFTER_DRIVES = $(QEMU_COMPETITION_AFTER_DRIVES) -device virtio-rng-pci
BUILDSTORM_PRODUCT_ROOT ?= $(PRODUCT_ROOT)/buildstorm
BUILDSTORM_KERNEL_OUTPUT_ROOT ?= $(BUILDSTORM_PRODUCT_ROOT)/kernel
BUILDSTORM_USER_OUTPUT_ROOT ?= $(BUILDSTORM_PRODUCT_ROOT)/user
BUILDSTORM_KERNEL_LA ?= $(BUILDSTORM_KERNEL_OUTPUT_ROOT)/kernel-la
QEMU_DEVELOPMENT_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(KERNEL_ELF)
QEMU_DEVELOPMENT_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_REGRESSION_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(KERNEL_ELF)
QEMU_REGRESSION_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS) $(NET_DEV)
# LoongArch QEMU loads the ELF directly.  Unlike RV64, this architecture has
# no BOOTLOADER value; pairing an empty `-bios` with `-device loader,...`
# makes QEMU consume the loader device text as a firmware filename.
QEMU_KTEST_BEFORE_DRIVES = -kernel $(KERNEL_ELF)
QEMU_KTEST_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_KTEST_X0 = $(KTEST_EXT4_IMAGE)

BOARD_2K1000_ARTIFACT_ROOT ?= $(PRODUCT_ROOT)/board/2k1000
BOARD_2K1000_TEST_CONFIG ?= $(abspath ../os_test.conf)
LA64_LINKER_RUSTFLAGS = -C link-arg=-nostdlib -C link-arg=-static -C force-frame-pointers=yes -C link-arg=-T$(abspath $(LINKER_SCRIPT))

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
	@cd ../user && make rust-user ARCH=la64 MODE=$(MODE) USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)"

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: toolchain-preflight user
	@mkdir -p $(dir $(ROOTFS_IMG))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_rootfs.sh "$(ROOTFS_IMG)" "$(BOARD)" $(MODE) $(FS_MODE)

kernel: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_LA)

$(INITRAMFS_CPIO_LA): user
	@mkdir -p $(dir $(INITRAMFS_CPIO_LA))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)

$(REGRESSION_CPIO_LA): user
	@mkdir -p $(dir $(REGRESSION_CPIO_LA))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh la64 $(MODE) $(REGRESSION_CPIO_LA) regression

$(KTEST_CPIO_LA): user
	@mkdir -p $(dir $(KTEST_CPIO_LA))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh la64 $(MODE) $(KTEST_CPIO_LA) ktest

kernel: $(LWEXT4_LA_PREREQ)
	@echo Platform: $(BOARD)
	@test -f $(LINKER_SCRIPT) || { echo "missing linker script: $(LINKER_SCRIPT)" >&2; exit 1; }
ifeq ($(MODE), debug)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" RUSTFLAGS="$(LA64_LINKER_RUSTFLAGS)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) cargo build --features "$(BOOT_PROFILE) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
else
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" RUSTFLAGS="$(LA64_LINKER_RUSTFLAGS)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) cargo build --release --features "$(BOOT_PROFILE) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
endif

# uImage (la64-specific: for uboot boot)
uimage: $(KERNEL_BIN)
	@mkdir -p $(dir $(KERNEL_UIMG))
	../util/mkimage -A loongarch -O linux -T kernel -C none \
	  -a $(LA_LOAD_ADDR) -e $(LA_ENTRY_POINT) \
	  -n NPUcore+ -d $(KERNEL_BIN) $(KERNEL_UIMG)

# Real-board images retain the historical target names, but build only through
# the ARCH=la64 parameterized makefile and declared product/image-role inputs.
la64-2k1000-run-clean:
	@grep -Eq '^mode=run$$' "$(BOARD_2K1000_TEST_CONFIG)" || { echo "os_test.conf must contain mode=run" >&2; exit 1; }
	@$(MAKE) ARCH=la64 PROFILE=normal -f $(firstword $(MAKEFILE_LIST)) uimage BOARD=2k1000 BLK_MODE=sata MODE=$(MODE) LOG=off KERNEL_UIMG="$(BOARD_2K1000_ARTIFACT_ROOT)/kernel-2k1000-run.ui"

la64-2k1000-core-tests:
	@grep -Eq '^mode=run$$' "$(BOARD_2K1000_TEST_CONFIG)" || { echo "os_test.conf must contain mode=run" >&2; exit 1; }
	@$(MAKE) ARCH=la64 PROFILE=normal -B -f $(firstword $(MAKEFILE_LIST)) uimage BOARD=2k1000 BLK_MODE=sata MODE=$(MODE) LOG=off EXTRA_FEATURES="sata_scratch_rw" KERNEL_UIMG="$(BOARD_2K1000_ARTIFACT_ROOT)/kernel-2k1000-core-tests.ui"

la64-2k1000-shell:
	@$(MAKE) ARCH=la64 PROFILE=normal -B -f $(firstword $(MAKEFILE_LIST)) uimage BOARD=2k1000 BLK_MODE=virt MODE=$(MODE) LOG=off EXTRA_FEATURES="gmac_2k1000" KERNEL_UIMG="$(BOARD_2K1000_ARTIFACT_ROOT)/kernel-2k1000-shell.ui"

la64-2k1000-apk-persist-shell:
	@$(MAKE) ARCH=la64 PROFILE=normal -B -f $(firstword $(MAKEFILE_LIST)) uimage BOARD=2k1000 BLK_MODE=sata MODE=$(MODE) LOG=off APK_RUNTIME=1 EXTRA_FEATURES="sata_scratch_rw p4_persist_rw gmac_dhcp apk_persist_shell" KERNEL_UIMG="$(BOARD_2K1000_ARTIFACT_ROOT)/kernel-2k1000-persist-shell.ui"

clean:
	@rm -rf "$(KERNEL_OUTPUT_ROOT)" "$(LWEXT4_LA_OUTPUT_DIR)"

# ============================================================
# QEMU run targets
# ============================================================

check-development-x0:
	@python3 ../scripts/image_roles.py validate-mutable --repo-root .. --arch la64 --path "$(IMAGE_ROLE_LA64_DEVELOPMENT_X0)" >/dev/null

run: toolchain-preflight build check-development-x0
ifeq ($(BOARD), laqemu)
	@$(call qemu_profile_command,development)
endif

runsimple: toolchain-preflight check-development-x0
	@$(call qemu_profile_command,development)

comp: toolchain-preflight
	@$(call qemu_profile_command,competition)

derived-comp: toolchain-preflight
	@python3 ../scripts/image_roles.py validate-derived --repo-root .. --arch la64 --path "$(IMAGE_ROLE_LA64_DERIVED_X0)" >/dev/null
	@$(call qemu_profile_command,derived-competition)

comp-gdb: toolchain-preflight
	@$(call qemu_competition_gdb_command)

buildstorm: toolchain-preflight buildstorm-input
	@$(MAKE) -B -f $(firstword $(MAKEFILE_LIST)) build \
		PRODUCT_ROOT="$(BUILDSTORM_PRODUCT_ROOT)" \
		KERNEL_OUTPUT_ROOT="$(BUILDSTORM_KERNEL_OUTPUT_ROOT)" \
		USER_OUTPUT_ROOT="$(BUILDSTORM_USER_OUTPUT_ROOT)" \
		KERNEL_CMDLINE="$(BUILDSTORM_CMDLINE)"
	@$(call qemu_profile_command,buildstorm)

buildstorm-input:
	@set -eu; \
		mkdir -p "$(dir $(BUILDSTORM_GOLDEN_X0))"; \
		if [ ! -f "$(BUILDSTORM_GOLDEN_X0)" ]; then \
			archive="$(BUILDSTORM_ARCHIVE)"; \
			[ -n "$$archive" ] && [ -f "$$archive" ] || { echo "[buildstorm] missing LA64 pub archive (set BUILDSTORM_ARCHIVE)" >&2; exit 1; }; \
			tmp="$(BUILDSTORM_GOLDEN_X0).tmp.$$$$"; \
			echo "[buildstorm] expanding official LA64 image once: $$archive"; \
			gzip -dc "$$archive" > "$$tmp"; \
			mv -f "$$tmp" "$(BUILDSTORM_GOLDEN_X0)"; \
		else \
			echo "[buildstorm] reusing official LA64 golden image $(BUILDSTORM_GOLDEN_X0)"; \
		fi; \
		rm -f "$(QEMU_BUILDSTORM_X0)"; \
		qemu-img create -q -f qcow2 -F raw -b "$(abspath $(BUILDSTORM_GOLDEN_X0))" "$(QEMU_BUILDSTORM_X0)"

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb env toolchain-preflight check ktest-build-only check-development-x0 derived-comp la64-2k1000-run-clean la64-2k1000-core-tests la64-2k1000-shell la64-2k1000-apk-persist-shell

ifeq ($(MODE),release)
CHECK_RELEASE_FLAG := --release
endif

check: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_LA)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" RUSTFLAGS="$(LA64_LINKER_RUSTFLAGS)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=$(LOG) \
		cargo check $(CHECK_RELEASE_FLAG) --features "$(BOOT_PROFILE) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
ktest-build-only: toolchain-preflight user $(KTEST_CPIO_LA) $(LWEXT4_LA_PREREQ)
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" RUSTFLAGS="$(LA64_LINKER_RUSTFLAGS)" MANGO_CMDLINE="$(KTEST_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KTEST_CPIO_LA))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} \
		cargo build --release --features "$(BOOT_PROFILE) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)

ktest-run: toolchain-preflight ktest-build-only ktest-clean-ext4
	@if [ "x$(KTEST_FIXTURE)" = "xborrows-initproc" ]; then \
		echo "[ktest-fixture] borrows-initproc: checking ktest is independent of INITPROC.process..."; \
		ktest_refs=$$(grep -n 'INITPROC\.process' ../os/src/task/mod.rs ../os/src/task/task.rs 2>/dev/null | grep -i 'spawn_ktest\|new_ktest\|ktest_trampoline\|zombify_ktest\|KTEST'); \
		if [ -n "$$ktest_refs" ]; then \
			echo "FAIL: KTEST_FIXTURE=borrows-initproc — INITPROC.process referenced in ktest code path:" >&2; \
			echo "$$ktest_refs" >&2; exit 1; \
		fi; \
		echo "PASS: KTEST_FIXTURE=borrows-initproc — ktest is independent of INITPROC"; \
	fi
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)
	@echo "[ktest] Launching QEMU (timeout: ${KTEST_QEMU_TIMEOUT}s)..."
	@timeout --foreground ${KTEST_QEMU_TIMEOUT} $(call qemu_profile_command,ktest) >/tmp/ktest-la.log 2>&1; \
	qemu_status=$$?; \
	cat /tmp/ktest-la.log; \
	test $$qemu_status -eq 0 && test -s /tmp/ktest-la.log && grep -Fq "[KTEST RESULT: PASS]" /tmp/ktest-la.log \
		&& echo "=== KTEST PASS ===" \
		|| (echo "=== KTEST FAIL ===" >&2; exit 1)

regression-run: toolchain-preflight
	@echo "[regression] Building la64 kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU (no disks, timeout 60s)..."
	@timeout --foreground 120 $(call qemu_profile_command,regression) >/tmp/regression-la.log 2>&1; \
	qemu_status=$$?; \
	cat /tmp/regression-la.log; \
	state=$$(../scripts/check-la64-regression-log.sh /tmp/regression-la.log $$qemu_status); \
	printf '%s\n' "$$state"; \
	case "$$state" in \
		"STATE=PASS STATUS=0") echo "=== REGRESSION PASS ==="; exit 0 ;; \
		"STATE=BLOCKED_STAGE1_PRE_ENTRY STATUS="*|"STATE=BLOCKED_STAGE1_POST_ENTRY STATUS="*) echo "=== REGRESSION BLOCKED ==="; exit 1 ;; \
		*) echo "=== REGRESSION FAIL ==="; exit 1 ;; \
	esac
