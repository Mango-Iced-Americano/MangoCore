include make/common/toolchain.mk
include make/image-roles.mk
include make/arch/rv64-settings.mk
include make/common/orchestration.mk
include make/qemu-profiles.mk

QEMU_EXECUTABLE = qemu-system-riscv64
QEMU_ROLE_ARCH = RV64
QEMU_COMPETITION_X0 = $(IMAGE_ROLE_RV64_COMPETITION_X0)
QEMU_DERIVED_X0 = $(IMAGE_ROLE_RV64_DERIVED_X0)
QEMU_DEVELOPMENT_X0 = $(IMAGE_ROLE_RV64_DEVELOPMENT_X0)
QEMU_BUILDSTORM_X0 = $(IMAGE_ROLE_RV64_BUILDSTORM_X0)
BUILDSTORM_GOLDEN_X0 = $(IMAGE_ROLE_RV64_BUILDSTORM_GOLDEN_X0)
BUILDSTORM_ARCHIVE = $(IMAGE_ROLE_RV64_BUILDSTORM_ARCHIVE)
QEMU_COMPETITION_BEFORE_DRIVES = -kernel $(KERNEL_IMAGE) -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS) -bios default
QEMU_COMPETITION_AFTER_DRIVES = -no-reboot -rtc base=utc $(NET_DEV) -object filter-dump,id=f1,netdev=net,file=packets.pcap
QEMU_COMPETITION_GDB_BEFORE_DRIVES = $(QEMU_COMPETITION_BEFORE_DRIVES)
QEMU_COMPETITION_GDB_AFTER_DRIVES = $(QEMU_COMPETITION_AFTER_DRIVES) -S -s
QEMU_BUILDSTORM_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(BUILDSTORM_KERNEL_RV) -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS) -bios default
QEMU_BUILDSTORM_AFTER_DRIVES = $(QEMU_COMPETITION_AFTER_DRIVES) -device virtio-rng-device,bus=virtio-mmio-bus.2
BUILDSTORM_PRODUCT_ROOT ?= $(PRODUCT_ROOT)/buildstorm
BUILDSTORM_KERNEL_OUTPUT_ROOT ?= $(BUILDSTORM_PRODUCT_ROOT)/kernel
BUILDSTORM_USER_OUTPUT_ROOT ?= $(BUILDSTORM_PRODUCT_ROOT)/user
BUILDSTORM_KERNEL_RV ?= $(BUILDSTORM_KERNEL_OUTPUT_ROOT)/Image
QEMU_DEVELOPMENT_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(KERNEL_IMAGE) -bios default
QEMU_DEVELOPMENT_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_REGRESSION_BEFORE_DRIVES = $(QEMU_MTTCG_ARGS) -kernel $(KERNEL_IMAGE) -bios default
QEMU_REGRESSION_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS) $(NET_DEV)
QEMU_KTEST_BEFORE_DRIVES = $(QEMU_REGRESSION_BEFORE_DRIVES)
QEMU_KTEST_AFTER_DRIVES = -m $(QEMU_MEMORY) $(QEMU_SMP_ARGS)
QEMU_KTEST_X0 = $(KTEST_EXT4_IMAGE)

lwext4-rv64: $(LWEXT4_RV_LIB)

$(LWEXT4_RV_PREPARED): $(LWEXT4_RV_INPUTS)
	@rm -rf $(LWEXT4_RV_SOURCE_DIR) $(LWEXT4_RV_BUILD_DIR)
	@mkdir -p $(LWEXT4_RV_SOURCE_DIR)
	@tar -C $(LWEXT4_DIR) --exclude='build_*' -cf - . | tar -C $(LWEXT4_RV_SOURCE_DIR) -xf -
	@cp -f ../dependency/lwext4_rust/c/ulibc.c $(LWEXT4_RV_SOURCE_DIR)/src/ulibc.c
	@touch $@

$(LWEXT4_RV_LIB): $(LWEXT4_RV_PREPARED)
	@echo "=== Building lwext4 C library for riscv64 ==="
	@ARCH=riscv64 cmake -G"Unix Makefiles" \
			-DCMAKE_BUILD_TYPE=Release \
			-DVERSION_MAJOR=1 -DVERSION_MINOR=0 -DVERSION_PATCH=0 \
			-DLWEXT4_BUILD_SHARED_LIB=OFF \
			-DLIB_ONLY=TRUE \
			-DCMAKE_TOOLCHAIN_FILE=$(abspath $(LWEXT4_CMAKE)) \
			-S $(LWEXT4_RV_SOURCE_DIR) -B $(LWEXT4_RV_BUILD_DIR)
	@cmake --build $(LWEXT4_RV_BUILD_DIR) --target lwext4 --parallel $$(nproc)
	@cp -f $(LWEXT4_RV_BUILD_DIR)/src/liblwext4.a $(LWEXT4_RV_LIB)
	@echo "=== lwext4 riscv64 .a built at $(LWEXT4_RV_LIB) ==="

clean-lwext4-rv:
	@rm -rf $(LWEXT4_RV_OUTPUT_DIR)

all: toolchain-preflight $(if $(filter 1,$(BUILD_ROOTFS)),fs-img,) build

debug: build mv-debug

# 评测机只认根目录 kernel-rv。RV64 内核 ELF 的 PhysAddr 是链接虚拟地址
# （0xffffffc0...），QEMU 无法按 program header 加载，因此必须发布 strip 后的
# raw binary（与 Image 同源），而不是带 debuginfo 的 ELF。
stage-kernel: kernel
	@mkdir -p $(dir $(PRODUCT_ROOT)/kernel/kernel-rv)
	cp -f $(KERNEL_IMAGE) $(PRODUCT_ROOT)/kernel/kernel-rv

mv: stage-kernel
	@echo "[deprecated] mv stages the RV64 kernel; root publication happens only after make all succeeds"

mv-debug:
	cp -f $(KERNEL_ELF) ../kernel-rv

build: env stage-kernel

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight

# build all user programs
user: toolchain-preflight
	@cd ../user && make rust-user ARCH=rv64 MODE=$(MODE) USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)"

$(APPS):

fs-img: toolchain-preflight user
	@mkdir -p $(dir $(ROOTFS_IMG))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_rootfs.sh "$(ROOTFS_IMG)" "rv64" $(MODE) $(FS_MODE)

kernel: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_RV)

$(INITRAMFS_CPIO_RV): user
	@mkdir -p $(dir $(INITRAMFS_CPIO_RV))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh rv64 $(MODE) $(INITRAMFS_CPIO_RV)

$(REGRESSION_CPIO_RV): user
	@mkdir -p $(dir $(REGRESSION_CPIO_RV))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh rv64 $(MODE) $(REGRESSION_CPIO_RV) regression

$(KTEST_CPIO_RV): user
	@mkdir -p $(dir $(KTEST_CPIO_RV))
	USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)" ../scripts/build_initramfs.sh rv64 $(MODE) $(KTEST_CPIO_RV) ktest

kernel: $(LWEXT4_PREREQ)
    ifeq ($(MODE), debug)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_RV))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} cargo build --features "riscv $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    else
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_RV))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} cargo build --release --features "riscv $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    endif
	@mkdir -p $(dir $(KERNEL_IMAGE))
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_IMAGE)

clean:
	@rm -rf "$(KERNEL_OUTPUT_ROOT)" "$(LWEXT4_RV_OUTPUT_DIR)"

check-development-x0:
	@python3 ../scripts/image_roles.py validate-mutable --repo-root .. --arch rv64 --path "$(IMAGE_ROLE_RV64_DEVELOPMENT_X0)" >/dev/null

run: toolchain-preflight build check-development-x0
	@$(call qemu_profile_command,development)

monitor:
	riscv64-unknown-elf-gdb -ex 'file target/riscv64gc-unknown-none-elf/debug/os' -ex 'set arch riscv:rv64' -ex 'target remote localhost:1234'

gdb: check-development-x0
	@$(call qemu_profile_command,debug) | tee qemu.log

runsimple: toolchain-preflight check-development-x0
	@$(call qemu_profile_command,development)

comp: toolchain-preflight
	@$(call qemu_profile_command,competition)

derived-comp: toolchain-preflight
	@python3 ../scripts/image_roles.py validate-derived --repo-root .. --arch rv64 --path "$(IMAGE_ROLE_RV64_DERIVED_X0)" >/dev/null
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
			[ -n "$$archive" ] && [ -f "$$archive" ] || { echo "[buildstorm] missing RV64 pub archive (set BUILDSTORM_ARCHIVE)" >&2; exit 1; }; \
			tmp="$(BUILDSTORM_GOLDEN_X0).tmp.$$$$"; \
			echo "[buildstorm] expanding official RV64 image once: $$archive"; \
			gzip -dc "$$archive" > "$$tmp"; \
			mv -f "$$tmp" "$(BUILDSTORM_GOLDEN_X0)"; \
		else \
			echo "[buildstorm] reusing official RV64 golden image $(BUILDSTORM_GOLDEN_X0)"; \
		fi; \
		rm -f "$(QEMU_BUILDSTORM_X0)"; \
		qemu-img create -q -f qcow2 -F raw -b "$(abspath $(BUILDSTORM_GOLDEN_X0))" "$(QEMU_BUILDSTORM_X0)"

.PHONY: user env toolchain-preflight check ktest-build-only check-development-x0 derived-comp

ifeq ($(MODE),release)
CHECK_RELEASE_FLAG := --release
endif

check: toolchain-preflight $(KERNEL_INITRAMFS_CPIO_RV)
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KERNEL_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KERNEL_INITRAMFS_CPIO_RV))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} \
		cargo check $(CHECK_RELEASE_FLAG) --features "riscv $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
# The kernel needs initramfs cpio (embedded via .S), so user
# programs must be built first.
ktest-build-only: toolchain-preflight user $(KTEST_CPIO_RV) $(LWEXT4_PREREQ)
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" MANGO_CMDLINE="$(KTEST_CMDLINE)" MANGO_INITRAMFS_CPIO="$(abspath $(KTEST_CPIO_RV))" MANGO_USER_OUTPUT_ROOT="$(abspath $(USER_OUTPUT_ROOT))" MANGO_USER_OUTPUT_MODE="$(MODE)" LOG=${LOG} \
		cargo build --release --features "riscv $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
	@mkdir -p $(dir $(KERNEL_IMAGE))
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_IMAGE)

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
	@echo "[ktest] Launching QEMU (timeout: ${KTEST_QEMU_TIMEOUT}s)..."
	@timeout --foreground ${KTEST_QEMU_TIMEOUT} $(call qemu_profile_command,ktest) >/tmp/ktest-rv.log 2>&1; \
	qemu_status=$$?; \
	cat /tmp/ktest-rv.log; \
	test $$qemu_status -eq 0 && test -s /tmp/ktest-rv.log && grep -Fq "[KTEST RESULT: PASS]" /tmp/ktest-rv.log \
		&& echo "=== KTEST PASS ===" \
		|| (echo "=== KTEST FAIL ===" >&2; exit 1)

regression-run: toolchain-preflight
	@echo "[regression] Building kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU (no disks, timeout 120s)..."
	@timeout --foreground 120 $(call qemu_profile_command,regression) 2>&1 | tee /tmp/regression-rv.log
	@grep -q "L4 REGRESSION RESULT: PASS" /tmp/regression-rv.log \
		&& echo "=== REGRESSION PASS ===" \
		|| (echo "=== REGRESSION FAIL ===" && exit 1)
