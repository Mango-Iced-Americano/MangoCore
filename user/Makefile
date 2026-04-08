TARGET := riscv64gc-unknown-none-elf
MODE := debug
COMP ?= yes

ifeq ($(BOARD), 2k1000)
	TARGET := loongarch64-unknown-linux-gnu
endif
ifeq ($(BOARD), laqemu)
	TARGET := loongarch64-unknown-linux-gnu
endif
ifeq ($(BOARD), rvqemu)
	TARGET := riscv64gc-unknown-none-elf
endif

ARCH=${shell echo ${TARGET}|cut -d- -f1 |grep -o "^[a-zA-Z]\+[[:digit:]]*"}
ifeq ($(ARCH), loongarch64)
	BOARD ?= laqemu
endif

build: rust-user

env:
ifeq ($(TARGET), riscv64gc-unknown-none-elf)
	echo "a"
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
else ifeq ($(TARGET), loongarch64-unknown-linux-gnu)
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
endif
	-(rustup target list | grep "$(TARGET) (installed)") || rustup target add $(TARGET)
	-cargo install --list |grep cargo-binutils |grep "v0.2.0">/dev/null || cargo install cargo-binutils --vers ~0.2
	-rustup component add rust-src
	-rustup component add llvm-tools-preview

rust-user: env $(APPS)
ifeq ($(MODE),release)
	cargo build --target=$(TARGET) --release
else
	cargo build --target=$(TARGET)
endif

clean:
	@cargo clean

.PHONY: build rust-user clean