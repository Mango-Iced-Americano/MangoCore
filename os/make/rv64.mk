# Building
TARGET := riscv64gc-unknown-none-elf
MODE := release
KERNEL_ELF := target/$(TARGET)/$(MODE)/os
KERNEL_BIN := $(KERNEL_ELF).bin
DISASM_TMP := target/$(TARGET)/$(MODE)/asm
BLK_MODE := virt
FS_MODE ?= ext4
ROOTFS_IMG_NAME = rootfs-rv.img
ROOTFS_IMG_DIR := ../fs-img-dir
CORE_NUM := 1
LOG ?= off
KERNEL_RV := ../kernel-rv
KERNEL_LA := ../kernel-la
SDCARD_RV := ../sdcard-rv.img
SDCARD_LA := ../sdcard-la.img

ifeq ($(BOARD), vf2)
	ROOTFS_IMG := /dev/sdc
else
	ROOTFS_IMG := ${ROOTFS_IMG_DIR}/${ROOTFS_IMG_NAME}
endif

APPS := ../user/src/bin/*

# BOARD
BOARD ?= rvqemu
# xein TODO: 下面代码因sbi版本改变确定无用后需要进行缩减
SBI ?= opensbi-1.0
ifeq ($(BOARD), rvqemu)
	ifeq ($(SBI), rustsbi)
		BOOTLOADER := ../bootloader/$(SBI)-$(BOARD).bin
	else ifeq ($(SBI), default)
		BOOTLOADER := default
	else
		BOOTLOADER := ../bootloader/fw_payload.bin
	endif
else ifeq ($(BOARD), vf2)
	BOOTLOADER := ../bootloader/rustsbi-$(BOARD).bin
endif

ifndef LOG
	LOG_OPTION := "log_off"
else
	LOG_OPTION := "log_${LOG}"
endif

# KERNEL ENTRY
ifeq ($(BOARD), rvqemu)
	KERNEL_ENTRY_PA := 0x80200000
else ifeq ($(BOARD), vf2)
	KERNEL_ENTRY_PA := 0x80020000
endif

# Binutils from rustup's llvm-tools-preview component. This avoids depending on
# the cargo-binutils wrapper being preinstalled or downloaded during grading.
HOST_TRIPLE := $(shell rustc -vV | sed -n 's/^host: //p')
LLVM_TOOLS_DIR := $(shell rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin
OBJDUMP := $(LLVM_TOOLS_DIR)/rust-objdump --arch-name=riscv64
OBJCOPY := $(LLVM_TOOLS_DIR)/rust-objcopy --binary-architecture=riscv64

# Disassembly
DISASM ?= -x

all: fs-img build

debug: build mv-debug

mv:
	cp -f $(KERNEL_ELF) ../kernel-rv

mv-debug:
	cp -f $(KERNEL_ELF) ../kernel-rv

build: env $(KERNEL_BIN) mv

env:
	(rustup target list | grep "riscv64gc-unknown-none-elf (installed)") || rustup target add $(TARGET)
	rustup target add $(TARGET)
	rustup component add rust-src
	rustup component add llvm-tools-preview

# build all user programs
user:
	@cd ../user && make rust-user BOARD=$(BOARD) MODE=$(MODE)

$(KERNEL_BIN): kernel
	@$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

$(APPS):

fs-img: user
	./buildfs.sh "$(ROOTFS_IMG)" "rvqemu" $(MODE) $(FS_MODE)

# Initramfs cpio generation (only when initramfs feature is active)
INITRAMFS_CPIO_RV := ../fs-img-dir/initramfs-rv.cpio

ifneq (,$(findstring initramfs,$(EXTRA_FEATURES)))
  # kernel（cargo build）需要在编译前拿到 cpio
  kernel: $(INITRAMFS_CPIO_RV)
endif

$(INITRAMFS_CPIO_RV): user
	@mkdir -p ../fs-img-dir
	./build_initramfs.sh rv64 $(MODE) $(INITRAMFS_CPIO_RV)
	@touch src/initramfs-rv.S  # 强制 Cargo 重编译（.incbin 时间戳变化）

# xein TODO: 注意需要评估zero_init启用与否的影响
kernel:
	@echo Platform: $(BOARD)
	@cp -f src/hal/arch/riscv/linker-$(BOARD).ld src/hal/arch/riscv/linker.ld
    ifeq ($(MODE), debug)
		@LOG=${LOG} cargo build --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    else
		@LOG=${LOG} cargo build --release --features "board_$(BOARD) $(LOG_OPTION) block_$(BLK_MODE) oom_handler $(EXTRA_FEATURES)"
    endif

clean:
	@cargo clean
	@rm -rf $(KERNEL_RV)

run: build
ifeq ($(BOARD), rvqemu)
	@qemu-system-riscv64 \
  		-machine virt \
  		-nographic \
  		-bios $(BOOTLOADER) \
  		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
  		-drive if=none,file=$(ROOTFS_IMG),format=raw,id=x0 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  		-m 1024 \
  		-smp threads=$(CORE_NUM)
endif

monitor:
	riscv64-unknown-elf-gdb -ex 'file target/riscv64gc-unknown-none-elf/debug/os' -ex 'set arch riscv:rv64' -ex 'target remote localhost:1234'

gdb:
	@qemu-system-riscv64 \
	-machine virt \
	-nographic \
	-bios $(BOOTLOADER) \
	-device loader,file=target/riscv64gc-unknown-none-elf/debug/os,addr=0x80200000 \
	-drive file=$(ROOTFS_IMG),if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-m 1024 \
	-smp threads=$(CORE_NUM) -S -s | tee qemu.log

runsimple:
	@qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios $(BOOTLOADER) \
		-device loader,file=$(KERNEL_BIN),addr=$(KERNEL_ENTRY_PA) \
		-drive file=$(ROOTFS_IMG),if=none,format=raw,id=x0 \
		-m 1024 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
		-smp threads=$(CORE_NUM)

comp:
	@qemu-system-riscv64 \
		-machine virt \
		-kernel $(KERNEL_RV) \
		-m 1024 \
		-nographic \
		-smp 1 \
		-bios default \
		-drive file=$(SDCARD_RV),if=none,format=raw,id=x0 \
		-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
		-no-reboot \
		-rtc base=utc \
		-device virtio-net-device,netdev=net,bus=virtio-mmio-bus.7 -netdev user,id=net \
		-object filter-dump,id=f1,netdev=net,file=packets.pcap

comp-gdb:
	@qemu-system-riscv64 \
        -machine virt \
        -kernel $(KERNEL_RV) \
        -m 1024 \
        -nographic \
        -smp 1 \
        -bios default \
        -drive file=$(SDCARD_RV),if=none,format=raw,id=x0 \
        -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
        -no-reboot \
        -rtc base=utc \
		-device virtio-net-device,netdev=net,bus=virtio-mmio-bus.7 -netdev user,id=net \
		-object filter-dump,id=f1,netdev=net,file=packets.pcap \
        -S \
        -s

.PHONY: user
