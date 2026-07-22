include make/common/toolchain.mk
include make/common/orchestration.mk

.PHONY: all $(ARCHS) clean prepare-cargo-config toolchain-preflight env arch-build kernel user image run test check lint validate-run publish-compatibility
.NOTPARALLEL:

all: prepare-cargo-config
	$(MAKE) rv64_all
	$(MAKE) la64_all
	$(MAKE) publish-compatibility

include make/tools.mk
include make/initramfs.mk

arch-build: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal regression,$(PROFILE)),,$(error PROFILE must be normal or regression)),$(error PROFILE must be normal or regression))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk build MODE=$(MODE) LOG=$(LOG) KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)" INITRAMFS_PROFILE=$(PROFILE) BLK_MODE=$(if $(filter la64,$(ARCH)),$(LA64_BLK_MODE),$(BLK_MODE))

kernel: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal regression,$(PROFILE)),,$(error PROFILE must be normal or regression)),$(error PROFILE must be normal or regression))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk build MODE=$(MODE) LOG=$(LOG) KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)" INITRAMFS_PROFILE=$(PROFILE) BLK_MODE=$(if $(filter la64,$(ARCH)),$(LA64_BLK_MODE),$(BLK_MODE))

user: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal)),$(error PROFILE must be normal))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk user MODE=$(MODE) LOG=$(LOG) BLK_MODE=$(if $(filter la64,$(ARCH)),$(LA64_BLK_MODE),$(BLK_MODE))

image: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal)),$(error PROFILE must be normal))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk fs-img MODE=$(MODE) LOG=$(LOG) BLK_MODE=$(if $(filter la64,$(ARCH)),$(LA64_BLK_MODE),$(BLK_MODE))

toolchain-preflight:
	@if [ "$(CANONICAL_BUILD_FIXTURE)" = "second-stage-failure" ]; then :; else sh ../scripts/rustup-preflight.sh; fi

env: toolchain-preflight

prepare-cargo-config: toolchain-preflight

ifeq ($(CANONICAL_BUILD_FIXTURE),second-stage-failure)
rv64_all: prepare-cargo-config
	@mkdir -p $(BUILD_ROOT)/rv64/$(MODE)/normal
	@touch $(BUILD_ROOT)/rv64/$(MODE)/normal/fixture-rv64

la64_all: prepare-cargo-config
	@echo "[fixture] forcing LA64 stage failure"
	@exit 1
else
rv64_all: prepare-cargo-config tools-disk-rv
	@echo "Building rv64"
	$(MAKE) ARCH=rv64 PROFILE=normal -f make/rv64.mk all MODE=$(MODE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal/user" TOOLS_IMG_RV="$(BUILD_ROOT)/rv64/$(MODE)/normal/tools/disk.img"

la64_all: prepare-cargo-config tools-disk-la
	@echo "Building la64"
	$(MAKE) ARCH=la64 PROFILE=normal -f make/la64.mk all MODE=$(MODE) LOG=$(LOG) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/user" TOOLS_IMG_LA="$(BUILD_ROOT)/la64/$(MODE)/normal/tools/disk-la.img"
endif

publish-compatibility:
	@echo "[publish-compatibility] publishing both completed architecture products"
	@mkdir -p "$(COMPAT_OUTPUT_DIR)"
	cp -f "$(BUILD_ROOT)/rv64/$(MODE)/normal/kernel/kernel-rv" "$(COMPAT_OUTPUT_DIR)/kernel-rv"
	cp -f "$(BUILD_ROOT)/la64/$(MODE)/normal/kernel/kernel-la" "$(COMPAT_OUTPUT_DIR)/kernel-la"
	cp -f "$(BUILD_ROOT)/rv64/$(MODE)/normal/tools/disk.img" "$(COMPAT_OUTPUT_DIR)/disk.img"
	cp -f "$(BUILD_ROOT)/la64/$(MODE)/normal/tools/disk-la.img" "$(COMPAT_OUTPUT_DIR)/disk-la.img"

rv64-kernel-build-only: prepare-cargo-config
	$(MAKE) ARCH=rv64 PROFILE=$(PROFILE) -f make/rv64.mk build BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/user"

rv64-debug: toolchain-preflight
	make -f make/rv64.mk debug BLK_MODE=${BLK_MODE} MODE=debug

rv64-only: prepare-cargo-config
	make -f make/rv64.mk all BLK_MODE=${BLK_MODE}

rv64-run-only: toolchain-preflight
	make -f make/rv64.mk runsimple

rv64-run: prepare-cargo-config
	make -f make/rv64.mk comp

# Regression test: self-contained initramfs — NO external disks
rv64-regression: prepare-cargo-config
	make -f make/rv64.mk regression-run MODE=${MODE} LOG=${LOG}

la64-only: prepare-cargo-config
	make -f make/la64.mk all MODE=${MODE} LOG=${LOG}

la64-kernel-build-only: prepare-cargo-config
	$(MAKE) ARCH=la64 PROFILE=$(PROFILE) -f make/la64.mk build BLK_MODE=$(LA64_BLK_MODE) MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/user"

la64-debug: toolchain-preflight
	make -f make/la64.mk all MODE=debug	

la64-run-only: toolchain-preflight
	make -f make/la64.mk runsimple MODE=${MODE} LOG=${LOG}

la64-run: prepare-cargo-config
	make -f make/la64.mk comp MODE=${MODE} LOG=${LOG}

la64-regression: prepare-cargo-config
	make -f make/la64.mk regression-run MODE=${MODE} LOG=${LOG}

comp: toolchain-preflight
	make -f make/$(CURR_ARCH).mk comp

clean:
	@rm -rf "$(BUILD_ROOT)"
	for arch in $(ARCHS); do \
		make ARCH=$$arch -f make/$$arch.mk clean; \
	done
	cd ../user && make clean

laclean:
	make -f make/la64.mk clean

fs-img: toolchain-preflight
	make -f make/$(CURR_ARCH).mk fs-img FS_MODE=$(FS_MODE)

validate-run:
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal)),$(error PROFILE must be normal))

run: validate-run toolchain-preflight
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk run INITRAMFS_PROFILE=$(PROFILE)

test: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk regression-run MODE=$(MODE) PRODUCT_ROOT="$(PRODUCT_ROOT)" KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)"

check: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	CARGO_TARGET_DIR="$(KERNEL_OUTPUT_ROOT)" cargo check --target $(if $(filter rv64,$(ARCH)),riscv64gc-unknown-none-elf,loongarch64-unknown-linux-gnu)

lint: prepare-cargo-config
	$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
	cargo fmt --check

gdb: toolchain-preflight
	make -f make/$(CURR_ARCH).mk gdb

rv64-gdb: rv64-debug
	make -f make/rv64.mk comp-gdb

la64-gdb: la64-debug
	make -f make/la64.mk comp-gdb	

rvchange:
	rm -f ../sdcard-rv.img
	cp ../../imgbk/sdcard-rv.img ../sdcard-rv.img

lachange:
	rm -f ../sdcard-la.img
	cp ../../imgbk/sdcard-la.img ../sdcard-la.img

docker:
	docker compose up -d
	docker compose exec -it os-dev bash

conf-inject: prepare-cargo-config
	@arch_list="$(if $(filter file default,$(origin CONF_ARCH)),rv64 la64,$(CONF_ARCH))"; \
	for arch in $$arch_list; do \
		ARCH=$$arch BLK_MODE=$(CONF_BLK_MODE) CONF_FILE=$(CONF_FILE) IMAGE_PATH=$(CONF_IMAGE) AUTO_REBUILD_MEM=$(AUTO_REBUILD_MEM) MODE=$(MODE) LOG=$(LOG) ./inject_os_test_conf.sh; \
	done

la64-inject-runtime: toolchain-preflight
	@cd ../user && make rust-user BOARD=laqemu MODE=$(MODE)
	@cmd_file=$$(mktemp); \
	printf '%s\n' \
		'rm /initproc' \
		'rm /bin/bash' \
		'rm /bin/busybox' \
		'write ../user/target/loongarch64-unknown-linux-gnu/$(MODE)/initproc /initproc' \
		'write ./bash-la /bin/bash' \
		'write ./busybox-la /bin/busybox' \
		'stat /initproc' \
		'stat /bin/bash' \
		'stat /bin/busybox' > $$cmd_file; \
	debugfs -w -f $$cmd_file ../sdcard-la.img; \
	rm -f $$cmd_file

# 将所有用户态测试程序（user/src/bin/*.rs）注入到 sdcard-rv.img
inject-test: toolchain-preflight
	@cd ../user && make rust-user MODE=release BOARD=rvqemu 2>&1 | tail -5
	@echo "[inject] Updating test binaries in ../sdcard-rv.img..."
	@for testname in $$(ls ../user/src/bin/*.rs | xargs -n1 basename | sed 's/\.rs$$//'); do \
		debugfs -w -R "rm $$testname" ../sdcard-rv.img 2>/dev/null || true; \
		debugfs -w -R "write ../user/target/riscv64gc-unknown-none-elf/release/$$testname $$testname" ../sdcard-rv.img 2>&1; \
	done
	@echo "[inject] Done."

# ============================================================
# lwext4 targets
# ============================================================
lwext4-rv64:
	make -f make/rv64.mk lwext4-rv64

lwext4-la64:
	make -f make/la64.mk lwext4-la64

.PHONY: lwext4-rv64 lwext4-la64

rv64-ktest: prepare-cargo-config
	make -f make/rv64.mk ktest-run MODE=${MODE} LOG=${LOG}

la64-ktest: prepare-cargo-config
	make -f make/la64.mk ktest-run MODE=${MODE} LOG=${LOG}

# Composite: fast check + unit test + kernel test
.PHONY: bugscan
bugscan: check-fast rv64-ktest
	@echo "[bugscan] All checks passed."

# Fast static checks (no QEMU needed)
.PHONY: check-fast
check-fast: toolchain-preflight
	@echo "[check-fast] Running cargo check..."
	cargo check --features "board_rvqemu block_virt" 2>&1 | tail -1

# ─────────────────────────────────────────────────────────
#  Aggregate targets — serial execution (architecture-specific source state)
# ─────────────────────────────────────────────────────────

regression-all:
	@$(MAKE) rv64-regression
	@$(MAKE) la64-regression

ktest-all:
	@$(MAKE) rv64-ktest KTEST=all
	@$(MAKE) la64-ktest KTEST=all
