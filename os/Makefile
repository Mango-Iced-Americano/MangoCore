include make/common/toolchain.mk
include make/image-roles.mk
include make/common/orchestration.mk

export RUSTUP_AUTO_INSTALL := 0
unexport RUSTUP_TOOLCHAIN
RUSTUP_HOME ?= $(HOME)/.rustup
CARGO_HOME ?= $(HOME)/.cargo
export RUSTUP_HOME CARGO_HOME

# submission 交付分支默认不制作 tools 盘，消除 Alpine 包下载等额外网络依赖；
# 本地需要工具盘时显式 make BUILD_TOOLS_DISK=1 开启。
BUILD_TOOLS_DISK ?= 0

# 评测不需要 rootfs 盘（make all 只产出内核/initramfs），默认跳过；
# 本地需要时显式 make BUILD_ROOTFS=1 开启。
BUILD_ROOTFS ?= 0

.PHONY: all $(ARCHS) clean prepare-cargo-config toolchain-preflight toolchain-setup env arch-build kernel user image run test check lint buildstorm ktest-build-only ktest-run validate-run publish-compatibility rv64-ktest la64-ktest rv64-ktest-build-only la64-ktest-build-only rv64-derived-run la64-derived-run la64-2k1000-run-clean la64-2k1000-core-tests la64-2k1000-shell la64-2k1000-apk-persist-shell
.NOTPARALLEL:

all: prepare-cargo-config
	$(MAKE) rv64_all
	$(MAKE) la64_all
	$(MAKE) publish-compatibility

include make/tools.mk
include make/initramfs.mk

define validate-formal-inputs
$(if $(filter command line environment environment override,$(origin ARCH)),,$(error ARCH must be explicitly provided))
$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
$(if $(filter command line environment environment override,$(origin PROFILE)),,$(error PROFILE must be explicitly provided))
$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal regression,$(PROFILE)),,$(error PROFILE must be normal or regression)),$(error PROFILE must be normal or regression))
endef

arch-build: prepare-cargo-config
	$(call validate-formal-inputs)
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk build MODE=$(MODE) LOG=$(LOG) KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)" INITRAMFS_PROFILE=$(PROFILE) $(if $(filter la64,$(ARCH)),BLK_MODE=$(LA64_BLK_MODE))

kernel: prepare-cargo-config
	$(call validate-formal-inputs)
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk build MODE=$(MODE) LOG=$(LOG) KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)" INITRAMFS_PROFILE=$(PROFILE) $(if $(filter la64,$(ARCH)),BLK_MODE=$(LA64_BLK_MODE))

user: prepare-cargo-config
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk user MODE=$(MODE) LOG=$(LOG) $(if $(filter la64,$(ARCH)),BLK_MODE=$(LA64_BLK_MODE))

image: prepare-cargo-config
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk fs-img MODE=$(MODE) LOG=$(LOG) $(if $(filter la64,$(ARCH)),BLK_MODE=$(LA64_BLK_MODE))

toolchain-preflight:
	@if [ "$(CANONICAL_BUILD_FIXTURE)" = "second-stage-failure" ]; then :; else sh ../scripts/rustup-preflight.sh; fi

toolchain-setup:
	@sh ../scripts/rustup-setup.sh

env: toolchain-preflight

prepare-cargo-config: toolchain-preflight
	@if [ -d ../.git ]; then \
		if [ -n "$${GIT_SUBMODULE_PROXY:-}" ]; then \
			git -C .. -c "url.$$GIT_SUBMODULE_PROXY.insteadOf=https://github.com/" submodule update --init --recursive; \
		else \
			git -C .. submodule update --init --recursive; \
		fi; \
	fi

ifeq ($(CANONICAL_BUILD_FIXTURE),second-stage-failure)
rv64_all: prepare-cargo-config
	@mkdir -p $(BUILD_ROOT)/rv64/$(MODE)/normal
	@touch $(BUILD_ROOT)/rv64/$(MODE)/normal/fixture-rv64

la64_all: prepare-cargo-config
	@echo "[fixture] forcing LA64 stage failure"
	@exit 1
else
rv64_all: prepare-cargo-config $(if $(filter 1,$(BUILD_TOOLS_DISK)),tools-disk-rv)
	@echo "Building rv64"
	$(MAKE) ARCH=rv64 PROFILE=normal -f make/rv64.mk all MODE=$(MODE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/normal/user" TOOLS_IMG_RV="$(BUILD_ROOT)/rv64/$(MODE)/normal/tools/disk.img"

la64_all: prepare-cargo-config $(if $(filter 1,$(BUILD_TOOLS_DISK)),tools-disk-la)
	@echo "Building la64"
	$(MAKE) ARCH=la64 PROFILE=normal -f make/la64.mk all MODE=$(MODE) LOG=$(LOG) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/user" TOOLS_IMG_LA="$(BUILD_ROOT)/la64/$(MODE)/normal/tools/disk-la.img"
endif

publish-compatibility:
	@echo "[publish-compatibility] publishing both completed architecture products"
	@mkdir -p "$(COMPAT_OUTPUT_DIR)"
	cp -f "$(BUILD_ROOT)/rv64/$(MODE)/normal/kernel/Image" "$(COMPAT_OUTPUT_DIR)/Image"
	cp -f "$(BUILD_ROOT)/la64/$(MODE)/normal/kernel/kernel-la" "$(COMPAT_OUTPUT_DIR)/kernel-la"
	@if [ -f "$(BUILD_ROOT)/rv64/$(MODE)/normal/tools/disk.img" ]; then \
		cp -f "$(BUILD_ROOT)/rv64/$(MODE)/normal/tools/disk.img" "$(COMPAT_OUTPUT_DIR)/disk.img"; \
	fi
	@if [ -f "$(BUILD_ROOT)/la64/$(MODE)/normal/tools/disk-la.img" ]; then \
		cp -f "$(BUILD_ROOT)/la64/$(MODE)/normal/tools/disk-la.img" "$(COMPAT_OUTPUT_DIR)/disk-la.img"; \
	fi

rv64-kernel-build-only: prepare-cargo-config
	$(MAKE) ARCH=rv64 PROFILE=$(PROFILE) -f make/rv64.mk build MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/user"

rv64-debug: toolchain-preflight
	make -f make/rv64.mk debug BLK_MODE=${BLK_MODE} MODE=debug

rv64-only: prepare-cargo-config
	make -f make/rv64.mk all

rv64-run-only: toolchain-preflight
	make -f make/rv64.mk runsimple

rv64-run: prepare-cargo-config
	make -f make/rv64.mk comp

rv64-derived-run: prepare-cargo-config
	make -f make/rv64.mk derived-comp

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

la64-derived-run: prepare-cargo-config
	make -f make/la64.mk derived-comp MODE=${MODE} LOG=${LOG}

la64-regression: prepare-cargo-config
	make -f make/la64.mk regression-run MODE=${MODE} LOG=${LOG}

la64-2k1000-run-clean la64-2k1000-core-tests la64-2k1000-shell la64-2k1000-apk-persist-shell: prepare-cargo-config
	$(MAKE) ARCH=la64 PROFILE=normal -f make/la64.mk $@ MODE=$(MODE) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/normal/user"

comp: toolchain-preflight
	make -f make/$(CURR_ARCH).mk comp

clean:
	@rm -rf "$(BUILD_ROOT)"
	for arch in rv64 la64; do \
		$(MAKE) ARCH=$$arch -f make/$$arch.mk clean; \
	done
	$(MAKE) -C ../user clean USER_OUTPUT_ROOT="$(USER_OUTPUT_ROOT)"

laclean:
	make -f make/la64.mk clean

fs-img: toolchain-preflight
	make -f make/$(CURR_ARCH).mk fs-img FS_MODE=$(FS_MODE)

validate-run:
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))

run: validate-run toolchain-preflight
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk run INITRAMFS_PROFILE=$(PROFILE)

buildstorm: toolchain-preflight
	$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64))
	$(MAKE) ARCH=$(ARCH) PROFILE=normal -f make/$(ARCH).mk buildstorm MODE=$(MODE) CORE_NUM=$(CORE_NUM) QEMU_MEMORY=$(QEMU_MEMORY)

test: prepare-cargo-config
	$(call validate-formal-inputs)
	$(if $(filter regression,$(PROFILE)),,$(error PROFILE must be regression))
	$(MAKE) ARCH=$(ARCH) -f make/$(ARCH).mk regression-run MODE=$(MODE) PRODUCT_ROOT="$(PRODUCT_ROOT)" KERNEL_OUTPUT_ROOT="$(KERNEL_OUTPUT_ROOT)"

check: prepare-cargo-config
	$(call validate-formal-inputs)
	$(MAKE) ARCH=$(ARCH) PROFILE=$(PROFILE) -f make/$(ARCH).mk check MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) $(if $(filter la64,$(ARCH)),BLK_MODE=$(LA64_BLK_MODE)) PRODUCT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/user"

lint: prepare-cargo-config
	$(call validate-formal-inputs)
	@echo "[lint] Checking first-party warning regression for $(ARCH) $(MODE)..."
	@sh ../scripts/lint-check.sh --arch $(ARCH) --mode $(MODE)

# Check formatting (separate from lint gate; not part of T10 warning baseline)
lint-format: prepare-cargo-config
	cargo fmt --check

ktest-build-only: prepare-cargo-config
	$(call validate-formal-inputs)
	$(MAKE) ARCH=$(ARCH) PROFILE=$(PROFILE) -f make/$(ARCH).mk ktest-build-only MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/user"

ktest-run: prepare-cargo-config
	$(call validate-formal-inputs)
	$(MAKE) ARCH=$(ARCH) PROFILE=$(PROFILE) -f make/$(ARCH).mk ktest-run MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/$(ARCH)/$(MODE)/$(PROFILE)/user" KTEST_FIXTURE="$(KTEST_FIXTURE)"

gdb: toolchain-preflight
	make -f make/$(CURR_ARCH).mk gdb

rv64-gdb: rv64-debug
	make -f make/rv64.mk comp-gdb

la64-gdb: la64-debug
	make -f make/la64.mk comp-gdb	

rvchange:
	@echo "ERROR: official evaluator x0 is immutable; create a derived development image instead" >&2
	@exit 2

lachange:
	@echo "ERROR: official evaluator x0 is immutable; create a derived development image instead" >&2
	@exit 2

docker:
	docker compose up -d
	docker compose exec -it os-dev bash

conf-inject: prepare-cargo-config
	@arch_list="$(if $(filter file default,$(origin CONF_ARCH)),rv64 la64,$(CONF_ARCH))"; \
	for arch in $$arch_list; do \
		ARCH=$$arch BLK_MODE=$(CONF_BLK_MODE) CONF_FILE=$(CONF_FILE) IMAGE_PATH=$(CONF_IMAGE) AUTO_REBUILD_MEM=$(AUTO_REBUILD_MEM) MODE=$(MODE) LOG=$(LOG) ../scripts/inject_os_test_conf.sh; \
	done

la64-inject-runtime: toolchain-preflight
	@echo "ERROR: direct runtime injection into official evaluator x0 is disabled; use a derived development image" >&2
	@exit 2

# 将所有用户态测试程序（user/src/bin/*.rs）注入到 sdcard-rv.img
inject-test: toolchain-preflight
	@echo "ERROR: direct test injection into official evaluator x0 is disabled; use a derived development image" >&2
	@exit 2

# ============================================================
# lwext4 targets
# ============================================================
lwext4-rv64:
	make -f make/rv64.mk lwext4-rv64

lwext4-la64:
	make -f make/la64.mk lwext4-la64

.PHONY: lwext4-rv64 lwext4-la64

rv64-ktest: prepare-cargo-config
	$(MAKE) ARCH=rv64 PROFILE=$(PROFILE) -f make/rv64.mk ktest-run MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/user" KTEST_FIXTURE="$(KTEST_FIXTURE)"

rv64-ktest-build-only: prepare-cargo-config
	$(MAKE) ARCH=rv64 PROFILE=$(PROFILE) -f make/rv64.mk ktest-build-only MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/rv64/$(MODE)/$(PROFILE)/user"

la64-ktest: prepare-cargo-config
	$(MAKE) ARCH=la64 PROFILE=$(PROFILE) -f make/la64.mk ktest-run MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/user" KTEST_FIXTURE="$(KTEST_FIXTURE)"

la64-ktest-build-only: prepare-cargo-config
	$(MAKE) ARCH=la64 PROFILE=$(PROFILE) -f make/la64.mk ktest-build-only MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=$(PROFILE) PRODUCT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)" KERNEL_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/kernel" USER_OUTPUT_ROOT="$(BUILD_ROOT)/la64/$(MODE)/$(PROFILE)/user"

# Composite: fast check + unit test + kernel test
.PHONY: bugscan
bugscan: check-fast rv64-ktest
	@echo "[bugscan] All checks passed."

# Fast static checks (no QEMU needed)
.PHONY: check-fast
check-fast: toolchain-preflight
	@echo "[check-fast] Running cargo check..."
	$(MAKE) ARCH=rv64 PROFILE=normal -f make/rv64.mk check MODE=$(MODE) LOG=$(LOG) INITRAMFS_PROFILE=normal

# ─────────────────────────────────────────────────────────
#  Aggregate targets — serial execution (architecture-specific source state)
# ─────────────────────────────────────────────────────────

regression-all:
	@$(MAKE) rv64-regression
	@$(MAKE) la64-regression

ktest-all:
	@$(MAKE) rv64-ktest KTEST=all
	@$(MAKE) la64-ktest KTEST=all
