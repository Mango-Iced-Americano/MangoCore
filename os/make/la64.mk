# Building
TARGET := loongarch64-unknown-linux-gnu
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
KERNEL_UIMG := $(KERNEL_ELF).ui
BLK_MODE := virt_pci
FS_MODE ?= ext4
ROOTFS_IMG_NAME = rootfs-la.img
ROOTFS_IMG_DIR := ../fs-img-dir
CORE_NUM := 1
LOG ?= off
VIRTIO_RNG_DEVICE := -device virtio-rng-pci
KERNEL_LA := ../kernel-la
SDCARD_LA := ../sdcard-la.img
DISK_LA := ../disk-la.img
P4_QEMU_DISK := ../mango-2k1000la-p4-qemu.img

# Avoid rustup/toolchain and generated-file races when a caller supplies -j.
.NOTPARALLEL:

# ============================================================
# lwext4 C library (la64: uses pre-installed cross-compiler)
# ============================================================
LWEXT4_LA_DIR := ../dependency/lwext4_rust/c/lwext4
LWEXT4_LA_LIB := $(LWEXT4_LA_DIR)/liblwext4-loongarch64.a
LWEXT4_LA_TOOLCHAIN_PATH := /opt/gcc-13.2.0-loongarch64-linux-gnu/bin
# Prerequisites for archive rebuild invalidation
LWEXT4_LA_SRCS := $(wildcard $(LWEXT4_LA_DIR)/src/*.c)
LWEXT4_LA_HDRS := $(wildcard $(LWEXT4_LA_DIR)/include/*.h) $(wildcard $(LWEXT4_LA_DIR)/include/misc/*.h)
LWEXT4_LA_CMAKE_INPUTS := $(LWEXT4_LA_DIR)/CMakeLists.txt \
                          $(LWEXT4_LA_DIR)/src/CMakeLists.txt \
                          ../dependency/lwext4_rust/c/elf-linux-gnu.cmake \
                          ../dependency/lwext4_rust/c/ulibc.c

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

# BOARD
BOARD ?= laqemu
# 开发板型号同时决定链接地址和早期入口实现。这里必须保留独立的板级链接脚本来源；
# 如果静默复用上一次 QEMU/2K1000 构建遗留的 linker.ld，可能得到格式正确但绝对地址
# 不适用于当前机器的 ELF。
LINKER_SCRIPT := src/hal/arch/loongarch64/linker-$(BOARD).ld

# Logging
ifndef LOG
	LOG_OPTION := "log_off"
else
	LOG_OPTION := "log_${LOG}"
endif

# Kernel entry (for -device loader fallback)
KERNEL_ENTRY_PA := 0x9000000090000000

# Binutils (cross toolchain, not llvm)
OBJCOPY := loongarch64-linux-gnu-objcopy
OBJDUMP := loongarch64-linux-gnu-objdump
READELF := loongarch64-linux-gnu-readelf

# uImage 配置。
# 传统 uImage 的装载地址和入口字段只有 32 位。2K1000 的 U-Boot 会在跳转前
# 通过 DMW 将物理地址映射到 0x9000... 缓存别名，因此镜像头必须填写低物理地址。
ifeq ($(BOARD), 2k1000)
LA_LOAD_ADDR := 0x90000000
LA_ENTRY_POINT := 0x90000000
else
LA_LOAD_ADDR := 0x9000000090000000
LA_ENTRY_POINT := 0x9000000090000000
endif

# Applications
APPS := ../user/src/bin/*

# RootFS image
ifeq ($(BOARD), laqemu)
	ROOTFS_IMG := ${ROOTFS_IMG_DIR}/${ROOTFS_IMG_NAME}
endif

# ============================================================
# Targets (symmetric with rv64.mk)
# ============================================================

all: fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

mv-debug:
	cp -f $(KERNEL_ELF) $(KERNEL_LA)

build: env $(KERNEL_BIN) mv

env:
	(rustup target list | grep "$(TARGET) (installed)") || rustup target add $(TARGET)
	rustup component add rust-src

# Build all user programs
user:
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: user
	./buildfs.sh "$(ROOTFS_IMG)" "$(BOARD)" $(MODE) $(FS_MODE)

# Initramfs cpio generation — parameterized for normal / regression profiles
INITRAMFS_CPIO_LA := ../fs-img-dir/initramfs-la.cpio
REGRESSION_CPIO_LA := ../fs-img-dir/initramfs-regression-la.cpio
CURL_RUNTIME ?= 0
APK_RUNTIME ?= 0
INET_TEST_RUNTIME ?= 0
RNG_TEST_RUNTIME ?= 0
# Only P4-enabled images carry the strict persistent Python policy resources.
# Custom images can still override this explicitly on the make command line.
PERSIST_PYTHON_RUNTIME ?= $(if $(filter p4_persist_rw,$(EXTRA_FEATURES)),1,0)

INITRAMFS_PROFILE ?= normal
ifeq ($(INITRAMFS_PROFILE),regression)
  KERNEL_INITRAMFS_CPIO_LA := $(REGRESSION_CPIO_LA)
  INITRAMFS_PROFILE_FEATURES := regression_initramfs
else
  KERNEL_INITRAMFS_CPIO_LA := $(INITRAMFS_CPIO_LA)
  INITRAMFS_PROFILE_FEATURES :=
endif

KERNEL_CMDLINE ?= mango.mode=normal

$(INITRAMFS_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	CURL_RUNTIME=$(CURL_RUNTIME) APK_RUNTIME=$(APK_RUNTIME) \
		INET_TEST_RUNTIME=$(INET_TEST_RUNTIME) \
		RNG_TEST_RUNTIME=$(RNG_TEST_RUNTIME) \
		PERSIST_PYTHON_RUNTIME=$(PERSIST_PYTHON_RUNTIME) \
		./build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)
	@touch src/initramfs-la.S

$(REGRESSION_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh la64 $(MODE) $(REGRESSION_CPIO_LA) regression
	@touch src/initramfs-regression-la.S

# lwext4: always build C library (now the default ext4 backend)
export LWEXT4_LIB_DIR := $(abspath $(LWEXT4_LA_DIR))
LWEXT4_LA_PREREQ := lwext4-la64

kernel: $(KERNEL_INITRAMFS_CPIO_LA) $(LWEXT4_LA_PREREQ)
	@echo Platform: $(BOARD)
	# 在调用 rustc 前直接失败，避免继续使用过期的 linker.ld 编译。
	@test -f $(LINKER_SCRIPT) || { echo "missing linker script: $(LINKER_SCRIPT)" >&2; exit 1; }
	@cp -f $(LINKER_SCRIPT) src/hal/arch/loongarch64/linker.ld
ifeq ($(MODE), debug)
	@MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=$(LOG) cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
else
	@MANGO_CMDLINE="$(KERNEL_CMDLINE)" LOG=$(LOG) cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(INITRAMFS_PROFILE_FEATURES) $(EXTRA_FEATURES)" --target $(TARGET)
endif

# uImage (la64-specific: for uboot boot)
uimage: $(KERNEL_BIN)
	../util/mkimage -A loongarch -O linux -T kernel -C none \
	  -a $(LA_LOAD_ADDR) -e $(LA_ENTRY_POINT) \
	  -n MangoCore -d $(KERNEL_BIN) $(KERNEL_UIMG)

clean:
	@which cargo >/dev/null 2>&1 && cargo clean || true
	@rm -rf $(KERNEL_LA)
	@rm -rf $(LWEXT4_LA_DIR)/build_lwext4-la64 $(LWEXT4_LA_LIB)

# ============================================================
# QEMU run targets
# ============================================================

run: build
ifeq ($(BOARD), laqemu)
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(DISK_LA),format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		$(VIRTIO_RNG_DEVICE) \
		-m 1024 \
		-smp threads=$(CORE_NUM)
endif

runsimple:
	@qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-drive if=none,file=$(DISK_LA),format=raw,id=x1 \
		-device virtio-blk-pci,drive=x1 \
		$(VIRTIO_RNG_DEVICE) \
		-m 1024 \
		-smp threads=$(CORE_NUM)

comp:
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
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

# Focused HTTPS shell runner. The official disk is exposed through a QEMU
# snapshot so interactive TLS testing cannot modify the checked-in test image;
# all curl and CA files come from initramfs.
qemu-curl-shell:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel ../kernel-la-curl-shell \
		-m 1G \
		-nographic \
		-smp 1 \
		-drive file=$(SDCARD_LA),if=none,format=raw,id=x0,snapshot=on \
		-device virtio-blk-pci,drive=x0 \
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

# Automated APK gate. The package manager itself is embedded in initramfs and
# all disk writes stay in QEMU snapshot/RAM state.
qemu-apk-tests:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel ../kernel-la-apk-tests \
		-m 1G \
		-nographic \
		-smp 1 \
		-drive file=$(SDCARD_LA),if=none,format=raw,id=x0,snapshot=on \
		-device virtio-blk-pci,drive=x0 \
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

# Persistent P4 runner deliberately avoids snapshot=on. Re-running this target
# against the same fixture validates that the committed APK tree survives a
# complete kernel reboot.
qemu-apk-persist-tests:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel ../kernel-la-apk-persist-tests \
		-m 1G \
		-nographic \
		-smp 1 \
		-drive file=$(P4_QEMU_DISK),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

qemu-apk-persist-shell:
	@qemu-system-loongarch64 \
		-machine virt \
		-kernel ../kernel-la-apk-persist-shell \
		-m 1G \
		-nographic \
		-smp 1 \
		-drive file=$(P4_QEMU_DISK),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-device virtio-net-pci,netdev=net0 \
		-netdev user,id=net0 \
		-rtc base=utc

comp-gdb:
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
		$(VIRTIO_RNG_DEVICE) \
		-no-reboot \
		-rtc base=utc \
		-S \
		-s

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb \
	qemu-curl-shell qemu-apk-tests qemu-apk-persist-tests \
	qemu-apk-persist-shell ktest-run regression-run

# ─────────────────────────────────────────────────────────
#  L3 Kernel self-test (mango.mode=ktest)
# ─────────────────────────────────────────────────────────
# Rebuilds kernel with MANGO_CMDLINE env var, then launches QEMU.
KTEST_EXT4_IMG_LA ?= /tmp/mango-lwext4-ktest-la.img
KTEST_EXT4_FEATURES ?= ^has_journal
KTEST_EXT4_BLOCK_SIZE ?= 4096
KTEST_EXT4_REUSE ?= 0
KTEST_POST_FSCK ?= 1
.PHONY: ktest-ext4-image
ktest-ext4-image:
ifeq ($(KTEST_EXT4_REUSE),0)
	@truncate -s 64M $(KTEST_EXT4_IMG_LA)
	@mke2fs -q -t ext4 -F -b $(KTEST_EXT4_BLOCK_SIZE) -m 0 -O $(KTEST_EXT4_FEATURES) $(KTEST_EXT4_IMG_LA)
	@e2fsck -f -n $(KTEST_EXT4_IMG_LA) >/dev/null
else
	@test -f $(KTEST_EXT4_IMG_LA) || { echo "missing reusable ktest image: $(KTEST_EXT4_IMG_LA)" >&2; exit 1; }
endif

ktest-run: $(INITRAMFS_CPIO_LA) $(LWEXT4_LA_PREREQ) ktest-ext4-image
	@echo "[ktest] Rebuilding kernel with: $(KTEST_CMDLINE)"
	@test -f $(LINKER_SCRIPT) || { echo "missing linker script: $(LINKER_SCRIPT)" >&2; exit 1; }
	@cp -f $(LINKER_SCRIPT) src/hal/arch/loongarch64/linker.ld
ifeq ($(MODE), debug)
	@MANGO_CMDLINE="$(KTEST_CMDLINE)" LOG=${LOG} \
		cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
else
	@MANGO_CMDLINE="$(KTEST_CMDLINE)" LOG=${LOG} \
		cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
endif
	@echo "[ktest] Launching QEMU (timeout: ${KTEST_QEMU_TIMEOUT}s)..."
	@timeout --foreground ${KTEST_QEMU_TIMEOUT} qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive if=none,file=$(KTEST_EXT4_IMG_LA),format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-m 1024 \
		-smp threads=1
ifeq ($(KTEST_POST_FSCK),1)
	@e2fsck -f -n $(KTEST_EXT4_IMG_LA)
endif

# ─────────────────────────────────────────────────────────
#  L4 User-mode regression test (mango.mode=regression)
# ─────────────────────────────────────────────────────────
# Builds minimal initramfs with /init=regression_init and
# /regression. Launches QEMU with a disposable ext4 drive. Parses
# console for [L4 REGRESSION RESULT: PASS] / FAIL markers.
REGRESSION_CMDLINE := mango.mode=regression
REGRESSION_EXT4_IMG_LA ?= /tmp/mango-lwext4-regression-la.img
REGRESSION_LOG_LA ?= /tmp/regression-la.log
REGRESSION_STATUS_LA ?= /tmp/regression-la.status
.PHONY: regression-ext4-image regression-run

regression-ext4-image:
	@truncate -s 64M $(REGRESSION_EXT4_IMG_LA)
	@mke2fs -q -t ext4 -F -b 4096 -m 0 -O ^has_journal $(REGRESSION_EXT4_IMG_LA)
	@e2fsck -f -n $(REGRESSION_EXT4_IMG_LA) >/dev/null

regression-run: regression-ext4-image
	@echo "[regression] Building la64 kernel with regression initramfs..."
	@$(MAKE) -f $(firstword $(MAKEFILE_LIST)) build INITRAMFS_PROFILE=regression KERNEL_CMDLINE="$(REGRESSION_CMDLINE)" \
		BLK_MODE=$(BLK_MODE) MODE=$(MODE) LOG=${LOG}
	@echo "[regression] Launching QEMU with disposable ext4 fixture (timeout 60s)..."
	@{ timeout --foreground 60 qemu-system-loongarch64 \
		-machine virt \
		-nographic \
		-kernel $(KERNEL_ELF) \
		-drive file=$(REGRESSION_EXT4_IMG_LA),if=none,format=raw,id=x0 \
		-device virtio-blk-pci,drive=x0 \
		-m 1024 \
			-smp threads=1; echo "$$?" > $(REGRESSION_STATUS_LA); } \
			2>&1 | tee $(REGRESSION_LOG_LA)
	@e2fsck -f -n $(REGRESSION_EXT4_IMG_LA)
	@test "$$(cat $(REGRESSION_STATUS_LA))" -eq 0 \
		&& grep -q "L4 REGRESSION RESULT: PASS" $(REGRESSION_LOG_LA) \
		&& echo "=== REGRESSION PASS ===" \
		|| (echo "=== REGRESSION FAIL ===" && exit 1)
