TARGET := riscv64gc-unknown-none-elf
MODE := debug
COMP ?= yes

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
	@sh ../scripts/restore-cargo-vendor-checksums.sh restore ..
	@mkdir -p .cargo
	@test -f .cargo/config.toml || cp -f ../cargo-config/user/config.toml .cargo/config.toml

toolchain-preflight:
	@sh ../scripts/rustup-preflight.sh

env: toolchain-preflight
ifeq ($(TARGET), riscv64gc-unknown-none-elf)
	@cp ./src/lang_items.rs.rv ./src/lang_items.rs
else ifeq ($(TARGET), loongarch64-unknown-linux-gnu)
	@cp ./src/lang_items.rs.la ./src/lang_items.rs
endif

rust-user: prepare-cargo-config env $(APPS)
ifeq ($(MODE),release)
	cargo build --target=$(TARGET) --release
else
	cargo build --target=$(TARGET)
endif

clean:
	@cargo clean

.PHONY: build rust-user clean prepare-cargo-config env toolchain-preflight
