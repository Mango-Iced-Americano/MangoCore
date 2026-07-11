MODE ?= release
FS_MODE ?= fat32
BLK_MODE ?= virt
DOCKER_IMAGE ?= docker.educg.net/cg/os-contest:20250614
LA_TOOLCHAIN ?= nightly-2024-05-01

QEMU_TAR := qemu-2k1000-static.20240526.tar.xz
QEMU_URL := https://gitlab.educg.net/wangmingjian/os-contest-2024-image/-/raw/master/$(QEMU_TAR)
QEMU_DIR := util/qemu-2k1000/tmp
QEMU_TAR_PATH := $(QEMU_DIR)/$(QEMU_TAR)

all:
	$(MAKE) prepare-cargo-config
	$(MAKE) clean
	$(MAKE) -C os all

prepare-cargo-config:
	@sh scripts/restore-cargo-vendor-checksums.sh restore .
	mkdir -p os/.cargo user/.cargo
	test -f os/.cargo/config.toml || cp -f cargo-config/os/config.toml os/.cargo/config.toml
	test -f user/.cargo/config.toml || cp -f cargo-config/user/config.toml user/.cargo/config.toml

env:
	rustup default $(LA_TOOLCHAIN)

kernel:
	cd os && make kernel

run: print-logo
	cd os && make run

runsimple:
	cd os && make runsimple

change-kernel-only:
	cd os && make build && make runsimple

print-logo:
	@echo "Welcome to NPUCore Project Aspera🚀"
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
.PHONY: all clean print-logo run run-simple qemu-download prepare-cargo-config

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
	make -C os clean

rv64-only:
	make -C os rv64-only BLK_MODE=${BLK_MODE}

regression:
	$(MAKE) -C os regression-all

# ── Testing shortcuts (run inside Docker container) ──
check-fast:
	cargo check -p mango-kernel-core
	cargo fmt --check -p mango-kernel-core
	cargo clippy -p mango-kernel-core 2>/dev/null || true

unittest:
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
	bash scripts/run_test_docker_parallel.sh

testsuits-download:
	cd fs-img-dir && \
	wget -O sdcard-la.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-la.img.xz && \
	wget -O sdcard-rv.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-rv.img.xz

	

.PHONY: all kernel run clean testsuits-download docker docker-test-parallel regression check-fast unittest bugscan
