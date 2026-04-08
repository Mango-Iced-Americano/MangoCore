MODE ?= release
FS_MODE ?= fat32
BLK_MODE ?= virt
DOCKER_IMAGE ?= docker.educg.net/cg/os-contest:20250614

QEMU_TAR := qemu-2k1000-static.20240526.tar.xz
QEMU_URL := https://gitlab.educg.net/wangmingjian/os-contest-2024-image/-/raw/master/$(QEMU_TAR)
QEMU_DIR := util/qemu-2k1000/tmp
QEMU_TAR_PATH := $(QEMU_DIR)/$(QEMU_TAR)

all: clean
	make -C os all

env:
	rustup default nightly-2024-05-01-x86_64-unknown-linux-gnu

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
.PHONY: all clean print-logo run run-simple qemu-download

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
	docker compose up -d && \
	docker compose exec -it os-dev bash

testsuits:
	mkdir -p testsuits && \
	cd testsuits && \
	[ -d os-contest-2024-image ] || git clone https://gitlab.educg.net/wangmingjian/os-contest-2024-image/ && \
	cd os-contest-2024-image && \
	if docker image inspect $(DOCKER_IMAGE) >/dev/null 2>&1; then \
		echo "Image $(DOCKER_IMAGE) already exists, skip docker build."; \
	else \
		docker build -t $(DOCKER_IMAGE) .; \
	fi && \
	cd .. && \
	[ -d testsuits-for-oskernel ] || git clone https://github.com/oscomp/testsuits-for-oskernel.git && \
	cd testsuits-for-oskernel && git checkout pre-2025 && \
	if [ -f sdcard-rv.img.xz ] && [ -f sdcard-la.img.xz ]; then \
		mv sdcard-rv.img.xz ../../fs-img-dir/ && \
		mv sdcard-la.img.xz ../../fs-img-dir/; \
	else \
		echo "sdcard image files not found. Run make sdcard first."; \
		docker run --rm -it -v .:/code --entrypoint bash -w /code --privileged $(DOCKER_IMAGE) -lc "make sdcard" && \
		mv sdcard-rv.img.xz ../../fs-img-dir/ && \
		mv sdcard-la.img.xz ../../fs-img-dir/; \
	fi

	

.PHONY: all kernel run clean testsuits docker
