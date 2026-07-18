MODE ?= release
FS_MODE ?= fat32
BLK_MODE ?= virt
DOCKER_IMAGE ?= docker.educg.net/cg/os-contest:20250614
LA_TOOLCHAIN ?= nightly-2024-05-01
BOARD_NET_IFACE ?= en8
IMAGE ?=
P3_IMAGE ?= mango-2k1000la-cpython-tools-p3.img
P3_MANIFEST ?= $(P3_IMAGE).json
P3_VERIFY_FILE ?= user/tools/cpython/L7_filesystem.py
P3_BACKUP_ID ?=
P4_IMAGE ?= mango-2k1000la-state-p4.img
P4_MANIFEST ?= $(P4_IMAGE).json
P4_QEMU_DISK ?= mango-2k1000la-p4-qemu.img
P4_MBR_SOURCE ?= /private/tftpboot/mango-2k1000la-full-test-mbr.img
P4_DOCKER_IMAGE ?= zhouzhouyi/os-contest:20260104
BOARD_SERIAL_ARG = $(if $(BOARD_SERIAL),--serial $(BOARD_SERIAL),)
PYTHON_RUNTIME_BUILD_MODE ?= production

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
.PHONY: all clean print-logo run run-simple qemu-download prepare-cargo-config \
	2k1000-boot 2k1000-boot-check 2k1000-p3-backup 2k1000-cpython-p3-write \
	2k1000-p4-image 2k1000-p4-qemu-disk 2k1000-p4-preflight 2k1000-p4-write \
	cpython-la64-runtime-build cpython-la64-runtime-verify cpython-la64-runtime-install \
	2k1000-python-runtime-deploy

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

docker:
	@if docker compose ps --status running 2>/dev/null | grep -q os-dev; then \
		docker compose exec -it os-dev bash; \
	else \
		docker compose up -d && docker compose exec -it os-dev bash; \
	fi

docker-test-parallel:
	bash scripts/run_test_docker_parallel.sh

# Canonical LoongArch runtime.  The os/Makefile tools-cpython-la target uses
# the same verified installer, so QEMU, board tools images and explicit host
# provisioning cannot silently fall back to an unaligned Alpine runtime.
cpython-la64-runtime-build:
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/app" -w /app $(P4_DOCKER_IMAGE) \
		./scripts/build_cpython_runtime_la64_strict.sh

cpython-la64-runtime-verify: cpython-la64-runtime-build
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/app" -w /app $(P4_DOCKER_IMAGE) \
		python3 scripts/install_cpython_runtime_la64_strict.py \
		--artifact-index target/cpython-strict/artifacts/current.json --verify-only

cpython-la64-runtime-install: cpython-la64-runtime-build
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/app" -w /app $(P4_DOCKER_IMAGE) \
		python3 scripts/install_cpython_runtime_la64_strict.py \
		--artifact-index target/cpython-strict/artifacts/current.json \
		--dest user/tools/loongarch64/tests/cpython

# Publish only to P4 ext4. P3 /tools is never read or written by this flow.
2k1000-python-runtime-deploy: cpython-la64-runtime-verify
	@test -n "$(PERF_RUN_DIR)" || { echo "usage: make 2k1000-python-runtime-deploy PERF_RUN_DIR=<run-dir>" >&2; exit 2; }
	python3 scripts/deploy_cpython_runtime.py \
		--run-dir "$(PERF_RUN_DIR)" \
		--artifact-index target/cpython-strict/artifacts/current.json \
		--build-mode "$(PYTHON_RUNTIME_BUILD_MODE)" $(BOARD_SERIAL_ARG)

2k1000-boot:
	@test -n "$(IMAGE)" || { echo "usage: make 2k1000-boot IMAGE=<uImage>" >&2; exit 2; }
	python3 scripts/boot_2k1000_tftp.py \
		--interface $(BOARD_NET_IFACE) \
		--image "$(IMAGE)" $(BOARD_SERIAL_ARG)

2k1000-boot-check:
	@test -n "$(IMAGE)" || { echo "usage: make 2k1000-boot-check IMAGE=<uImage>" >&2; exit 2; }
	python3 scripts/boot_2k1000_tftp.py \
		--interface $(BOARD_NET_IFACE) \
		--image "$(IMAGE)" $(BOARD_SERIAL_ARG) \
		--no-host-config --check-only

2k1000-p3-backup:
	@test -n "$(PERF_RUN_DIR)" || { echo "usage: make 2k1000-p3-backup PERF_RUN_DIR=<run-dir> P3_BACKUP_ID=<id> CONFIRM_P3_START=0xA80800" >&2; exit 2; }
	@test -n "$(P3_BACKUP_ID)" || { echo "refusing P3 backup without P3_BACKUP_ID" >&2; exit 2; }
	@test "$(CONFIRM_P3_START)" = "0xA80800" || { \
		echo "refusing P3 backup: set CONFIRM_P3_START=0xA80800" >&2; exit 2; \
	}
	python3 scripts/backup_2k1000_p3.py \
		--run-dir "$(PERF_RUN_DIR)" \
		--backup-id "$(P3_BACKUP_ID)" \
		--confirm-p3-start "$(CONFIRM_P3_START)" $(BOARD_SERIAL_ARG)

2k1000-cpython-p3-write:
	@test -n "$(P3_BACKUP_ID)" || { \
		echo "refusing P3 write: first create a verified /persist backup and set P3_BACKUP_ID" >&2; exit 2; \
	}
	@test "$(CONFIRM_P3_START)" = "0xA80800" || { \
		echo "refusing P3 write: set CONFIRM_P3_START=0xA80800" >&2; exit 2; \
	}
	python3 scripts/write_2k1000_p3.py \
		--interface $(BOARD_NET_IFACE) \
		--image "$(P3_IMAGE)" \
		--manifest "$(P3_MANIFEST)" \
		--verify-file "$(P3_VERIFY_FILE)" \
		--backup-id "$(P3_BACKUP_ID)" \
		--confirm-p3-start "$(CONFIRM_P3_START)" $(BOARD_SERIAL_ARG)

2k1000-p4-image:
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/app" -w /app $(P4_DOCKER_IMAGE) \
		python3 scripts/make_2k1000_p4_ext4.py \
		--output "$(P4_IMAGE)" --force

2k1000-p4-qemu-disk:
	@test -f "$(P4_IMAGE)" -a -f "$(P4_MANIFEST)" || $(MAKE) 2k1000-p4-image
	docker run --rm --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/app" -w /app $(P4_DOCKER_IMAGE) \
		python3 scripts/make_2k1000_p4_qemu_disk.py \
		--p4-image "$(P4_IMAGE)" --output "$(P4_QEMU_DISK)" --force

2k1000-p4-preflight:
	@test -f "$(P4_IMAGE)" -a -f "$(P4_MANIFEST)" || { \
		echo "missing P4 image or manifest; run make 2k1000-p4-image" >&2; exit 2; \
	}
	python3 scripts/write_2k1000_p4.py \
		--interface $(BOARD_NET_IFACE) \
		--image "$(P4_IMAGE)" --manifest "$(P4_MANIFEST)" \
		--mbr-source "$(P4_MBR_SOURCE)" \
		--confirm-p4-start 0xC00800 --confirm-p4-end 0x1400800 \
		--confirm-disk-sectors 62533296 --preflight-only $(BOARD_SERIAL_ARG)

2k1000-p4-write:
	@test -f "$(P4_IMAGE)" -a -f "$(P4_MANIFEST)" || { \
		echo "missing P4 image or manifest; run make 2k1000-p4-image" >&2; exit 2; \
	}
	@test "$(CONFIRM_P4_START)" = "0xC00800" || { \
		echo "refusing P4 write: set CONFIRM_P4_START=0xC00800" >&2; exit 2; \
	}
	@test "$(CONFIRM_P4_END)" = "0x1400800" || { \
		echo "refusing P4 write: set CONFIRM_P4_END=0x1400800" >&2; exit 2; \
	}
	@test "$(CONFIRM_DISK_SECTORS)" = "62533296" || { \
		echo "refusing P4 write: set CONFIRM_DISK_SECTORS=62533296" >&2; exit 2; \
	}
	python3 scripts/write_2k1000_p4.py \
		--interface $(BOARD_NET_IFACE) \
		--image "$(P4_IMAGE)" --manifest "$(P4_MANIFEST)" \
		--mbr-source "$(P4_MBR_SOURCE)" \
		--confirm-p4-start "$(CONFIRM_P4_START)" \
		--confirm-p4-end "$(CONFIRM_P4_END)" \
		--confirm-disk-sectors "$(CONFIRM_DISK_SECTORS)" $(BOARD_SERIAL_ARG)

testsuits-download:
	cd fs-img-dir && \
	wget -O sdcard-la.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-la.img.xz && \
	wget -O sdcard-rv.img.xz https://github.com/oscomp/testsuits-for-oskernel/releases/download/pre-20250615/sdcard-rv.img.xz

	

.PHONY: all kernel run clean testsuits-download docker docker-test-parallel
