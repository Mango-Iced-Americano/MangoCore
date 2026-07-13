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
KERNEL_LA := ../kernel-la
SDCARD_LA := ../sdcard-la.img
DISK_LA := ../disk-la.img

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

# Initramfs cpio generation (always needed when feature is in Cargo defaults)
INITRAMFS_CPIO_LA := ../fs-img-dir/initramfs-la.cpio
CURL_RUNTIME ?= 0
INET_TEST_RUNTIME ?= 0

kernel: $(INITRAMFS_CPIO_LA)

$(INITRAMFS_CPIO_LA): user
	@mkdir -p ../fs-img-dir
	CURL_RUNTIME=$(CURL_RUNTIME) INET_TEST_RUNTIME=$(INET_TEST_RUNTIME) \
		./build_initramfs.sh la64 $(MODE) $(INITRAMFS_CPIO_LA)
	@touch src/initramfs-la.S

kernel:
	@echo Platform: $(BOARD)
	# 在调用 rustc 前直接失败，避免继续使用过期的 linker.ld 编译。
	@test -f $(LINKER_SCRIPT) || { echo "missing linker script: $(LINKER_SCRIPT)" >&2; exit 1; }
	@cp -f $(LINKER_SCRIPT) src/hal/arch/loongarch64/linker.ld
ifeq ($(MODE), debug)
	@LOG=$(LOG) cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
else
	@LOG=$(LOG) cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)" --target $(TARGET)
endif

# uImage (la64-specific: for uboot boot)
uimage: $(KERNEL_BIN)
	../util/mkimage -A loongarch -O linux -T kernel -C none \
	  -a $(LA_LOAD_ADDR) -e $(LA_ENTRY_POINT) \
	  -n MangoCore -d $(KERNEL_BIN) $(KERNEL_UIMG)

clean:
	@cargo clean
	@rm -rf $(KERNEL_LA)

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
		-no-reboot \
		-rtc base=utc \
		-S \
		-s

.PHONY: all build kernel fs-img user clean run runsimple comp comp-gdb
