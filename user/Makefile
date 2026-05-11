TARGET := riscv64gc-unknown-none-elf
MODE := debug
COMP ?= yes
RV_TOOLCHAIN ?= nightly-2025-01-18
LA_TOOLCHAIN ?= nightly-2024-05-01

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

env:
ifeq ($(TARGET), riscv64gc-unknown-none-elf)
	echo "a"
	@rustup override set $(RV_TOOLCHAIN)
else ifeq ($(TARGET), loongarch64-unknown-linux-gnu)
	@rustup override set $(LA_TOOLCHAIN)
endif
	-(rustup target list | grep "$(TARGET) (installed)") || rustup target add $(TARGET)
	-rustup component add rust-src
	-rustup component add llvm-tools-preview

rust-user: prepare-cargo-config env $(APPS)
ifeq ($(MODE),release)
	cargo build --target=$(TARGET) --release
else
	cargo build --target=$(TARGET)
endif

clean:
	@cargo clean

.PHONY: build rust-user clean prepare-cargo-config
