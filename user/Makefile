TARGET := riscv64gc-unknown-none-elf
MODE := debug
COMP ?= yes
USER_OUTPUT_ROOT ?= target

export RUSTUP_AUTO_INSTALL := 0
unexport RUSTUP_TOOLCHAIN

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

prepare-cargo-config:

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight

rust-user: prepare-cargo-config env $(APPS)
ifeq ($(MODE),release)
	CARGO_TARGET_DIR="$(USER_OUTPUT_ROOT)" cargo build --target=$(TARGET) --release
else
	CARGO_TARGET_DIR="$(USER_OUTPUT_ROOT)" cargo build --target=$(TARGET)
endif

clean:
	@cargo clean

.PHONY: build rust-user clean prepare-cargo-config env toolchain-preflight
