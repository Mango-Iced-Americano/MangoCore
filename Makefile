MODE ?= release
PROFILE ?= normal
REPO_ROOT := $(CURDIR)
BUILD_ROOT ?= $(REPO_ROOT)/build
COMPAT_OUTPUT_DIR ?= $(REPO_ROOT)
export BUILD_ROOT COMPAT_OUTPUT_DIR CANONICAL_BUILD_FIXTURE
FS_MODE ?= fat32
BLK_MODE ?= virt
DOCKER_IMAGE ?= docker.educg.net/cg/os-contest:20250614

export RUSTUP_AUTO_INSTALL := 0
unexport RUSTUP_TOOLCHAIN

ifeq ($(origin RUSTUP_HOME),undefined)
ifeq ($(strip $(HOME)),)
$(error HOME must be set and non-empty when RUSTUP_HOME is not supplied)
endif
endif
ifeq ($(origin CARGO_HOME),undefined)
ifeq ($(strip $(HOME)),)
$(error HOME must be set and non-empty when CARGO_HOME is not supplied)
endif
endif
RUSTUP_HOME ?= $(HOME)/.rustup
CARGO_HOME ?= $(HOME)/.cargo
ifeq ($(strip $(RUSTUP_HOME)),)
$(error RUSTUP_HOME must be set and non-empty)
endif
ifeq ($(strip $(CARGO_HOME)),)
$(error CARGO_HOME must be set and non-empty)
endif
export RUSTUP_HOME CARGO_HOME

define validate-formal-inputs
$(if $(filter command line environment environment override,$(origin ARCH)),,$(error ARCH must be explicitly provided))
$(if $(filter 1,$(words $(ARCH))),$(if $(filter rv64 la64,$(ARCH)),,$(error ARCH must be rv64 or la64)),$(error ARCH must be rv64 or la64))
$(if $(filter command line environment environment override,$(origin PROFILE)),,$(error PROFILE must be explicitly provided))
$(if $(filter 1,$(words $(PROFILE))),$(if $(filter normal regression,$(PROFILE)),,$(error PROFILE must be normal or regression)),$(error PROFILE must be normal or regression))
endef

QEMU_TAR := qemu-2k1000-static.20240526.tar.xz
QEMU_URL := https://gitlab.educg.net/wangmingjian/os-contest-2024-image/-/raw/master/$(QEMU_TAR)
QEMU_DIR := util/qemu-2k1000/tmp
QEMU_TAR_PATH := $(QEMU_DIR)/$(QEMU_TAR)

all: toolchain-setup
	$(MAKE) prepare-cargo-config
	$(MAKE) -C os all

build:
	$(call validate-formal-inputs)
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" arch-build

prepare-cargo-config:

toolchain-setup:
	@sh scripts/rustup-setup.sh

toolchain-preflight:
	@sh scripts/rustup-preflight.sh

env: toolchain-preflight

kernel: toolchain-preflight
	$(call validate-formal-inputs)
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" kernel

user: toolchain-preflight
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" user

image: toolchain-preflight
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" image

validate-run:
	$(call validate-formal-inputs)
	$(if $(filter normal,$(PROFILE)),,$(error PROFILE must be normal))

run: validate-run print-logo toolchain-preflight
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" run

test: toolchain-preflight
	$(call validate-formal-inputs)
	$(if $(filter regression,$(PROFILE)),,$(error PROFILE must be regression))
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" test

check: toolchain-preflight
	$(call validate-formal-inputs)
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" check

lint: toolchain-preflight
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(or $(PROFILE),normal)" "BUILD_ROOT=$(BUILD_ROOT)" lint

ktest-build-only: toolchain-preflight
	$(call validate-formal-inputs)
	$(MAKE) -C os "ARCH=$(ARCH)" "MODE=$(MODE)" "PROFILE=$(PROFILE)" "BUILD_ROOT=$(BUILD_ROOT)" ktest-build-only

.NOTPARALLEL: run

runsimple: toolchain-preflight
	cd os && make runsimple

change-kernel-only: toolchain-preflight
	cd os && make build && make runsimple

print-logo:
	@echo "Welcome to MangoCore Project Aspera🚀"
	@echo "                                                                            "
	@echo "  ________    ________    ________    _______     ________    ________      "
	@echo " |\   __  \  |\   ____\  |\   __  \  |\  ___ \   |\   __  \  |\   __  \     "
	@echo " \ \  \|\  \ \ \  \___|_ \ \  \|\  \ \ \   __/|  \ \  \|\  \ \ \  \|\  \    "
	@echo "  \ \   __  \ \ \_____  \ \ \   ____\ \ \  \_|/__ \ \   _  _\ \ \   __  \   "
	@echo "   \ \  \ \  \ \|____|\  \ \ \  \___|  \ \  \_|\ \ \ \  \\  \| \ \  \ \  \  "
	@echo "    \ \__\ \__\  ____\_\  \ \ \__\      \ \_______\ \ \__\\ _\  \ \__\ \__\ "
	@echo "     \|__|\|__| |\_________\ \|__|       \|_______|  \|__|\|__|  \|__|\|__| "
	@echo "                \|_________|                                                "
	@echo "                                                                            "
	@echo "                                                                            "
.PHONY: all build kernel user image run test check lint ktest-build-only clean print-logo run-simple qemu-download prepare-cargo-config toolchain-setup toolchain-preflight env validate-run

qemu-download: $(QEMU_DIR)/.extracted
	chmod +x util/mkimage
	chmod +x util/qemu-2k1000/gz/runqemu2k1000
	chmod +x $(QEMU_DIR)/qemu/bin/qemu-system-loongarch64
	mkdir -p fs-img-dir
	sudo chmod 777 fs-img-dir/

$(QEMU_DIR)/.extracted: $(QEMU_TAR_PATH)
	@echo "Extracting $(QEMU_TAR)..."
	cd $(QEMU_DIR) && tar xavf $(QEMU_TAR)
	rm -rf $(QEMU_DIR)/qemu/2k1000 \
		$(QEMU_DIR)/qemu/runqemu \
		$(QEMU_DIR)/qemu/README.md \
		$(QEMU_DIR)/qemu/include \
		$(QEMU_DIR)/qemu/var
	@touch $@

$(QEMU_TAR_PATH):
	@mkdir -p $(QEMU_DIR)
	@if [ -f $@ ]; then \
		if ! tar tf $@ >/dev/null 2>&1; then \
			echo "File $@ is corrupted. Deleting and re-downloading..."; \
			rm -f $@; \
			wget -q $(QEMU_URL) -P $(QEMU_DIR); \
		fi; \
	else \
		echo "Downloading $(QEMU_TAR)..."; \
		wget -q $(QEMU_URL) -P $(QEMU_DIR); \
	fi
	@if ! tar tf $@ >/dev/null 2>&1; then \
		echo "Download failed, please check network connection"; \
		exit 1; \
	fi

clean:
	$(MAKE) -C os "BUILD_ROOT=$(BUILD_ROOT)" clean
	rm -f "$(COMPAT_OUTPUT_DIR)/kernel-rv" \
		"$(COMPAT_OUTPUT_DIR)/kernel-la" \
		"$(COMPAT_OUTPUT_DIR)/disk.img" \
		"$(COMPAT_OUTPUT_DIR)/disk-la.img"
	rm -rf "$(BUILD_ROOT)"

rv64-only:
	make -C os rv64-only BLK_MODE=${BLK_MODE}

regression: toolchain-preflight
	$(MAKE) -C os regression-all

# ── Testing shortcuts (run inside Docker container) ──
check-fast: toolchain-preflight
	cargo check -p mango-kernel-core
	cargo fmt --check -p mango-kernel-core
	cargo clippy -p mango-kernel-core

unittest: toolchain-preflight
	cargo test -p mango-kernel-core

bugscan: unittest
	@echo "[bugscan] L1 passed, running L3 ktest..."
	make -C os rv64-ktest KTEST=all

docker:
	@if docker compose ps --status running 2>/dev/null | grep -q os-dev; then \
		docker compose exec -it os-dev bash; \
	else \
		docker compose up -d && docker compose exec -it os-dev bash; \
	fi

docker-test-parallel:
	@printf '%s\n' 'ERROR: docker-test-parallel is deprecated; run python3 scripts/run_full_test.py --serial inside Docker instead.' >&2
	@exit 64

test-docker-parallel:
	@printf '%s\n' 'ERROR: test-docker-parallel is deprecated; run python3 scripts/run_full_test.py --serial inside Docker instead.' >&2
	@exit 64

testsuits-download:
	cd fs-img-dir && \
	wget -O sdcard-la.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-la.img.xz && \
	wget -O sdcard-rv.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-rv.img.xz

	

.PHONY: all build kernel user image run test check lint clean testsuits-download docker docker-test-parallel test-docker-parallel regression check-fast unittest bugscan
