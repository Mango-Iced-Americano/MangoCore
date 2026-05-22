ARCHS = rv64
CURR_ARCH ?= la64
FS_MODE ?= ext4
TOP_BLK_MODE_ORIGIN := $(origin BLK_MODE)
BLK_MODE ?= virt
LA64_BLK_MODE ?= $(if $(filter undefined,$(TOP_BLK_MODE_ORIGIN)),virt_pci,$(BLK_MODE))
MODE ?= release
CONF_FILE ?= ../os_test.conf
CONF_ARCH ?= $(CURR_ARCH)
CONF_BLK_MODE ?= $(if $(filter la la64,$(CONF_ARCH)),virt_pci,$(BLK_MODE))
CONF_IMAGE ?=
AUTO_REBUILD_MEM ?= 1
RV_TOOLCHAIN ?= nightly-2025-01-18
LA_TOOLCHAIN ?= nightly-2024-05-01

.PHONY: all $(ARCHS) clean prepare-cargo-config

all: prepare-cargo-config rv64_all la64_all

prepare-cargo-config:
	@sh ../scripts/restore-cargo-vendor-checksums.sh restore ..
	@mkdir -p .cargo ../user/.cargo
	@test -f .cargo/config.toml || cp -f ../cargo-config/os/config.toml .cargo/config.toml
	@test -f ../user/.cargo/config.toml || cp -f ../cargo-config/user/config.toml ../user/.cargo/config.toml

rv64_all: prepare-cargo-config
	@echo "Building rv64"
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make ARCH=rv64 -f make/rv64.mk all MODE=${MODE}

la64_all: prepare-cargo-config
	@echo "Building la64"
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make ARCH=la64 -f make/la64o.mk all MODE=${MODE} LOG=${LOG}

rv64-kernel-build-only: prepare-cargo-config
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk build BLK_MODE=${BLK_MODE} MODE=${MODE} LOG=${LOG}

rv64-debug:
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk debug BLK_MODE=${BLK_MODE} MODE=debug

rv64-only: prepare-cargo-config
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk all BLK_MODE=${BLK_MODE}

rv64-run-only:
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk runsimple

rv64-run: prepare-cargo-config
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk comp

la64-only: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk all MODE=${MODE} LOG=${LOG}

la64-kernel-build-only: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk build BLK_MODE=${LA64_BLK_MODE} MODE=${MODE} LOG=${LOG}

la64-debug:
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk all MODE=debug	

la64-run-only:
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk runsimple MODE=${MODE} LOG=${LOG}

la64-run: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk comp MODE=${MODE} LOG=${LOG}

comp:
	make -f make/$(CURR_ARCH).mk comp

clean:
	for arch in $(ARCHS); do \
		make ARCH=$$arch -f make/$$arch.mk clean; \
	done
	cd ../user && make clean

laclean:
	make -f make/la64.mk clean

fs-img:
	make -f make/$(CURR_ARCH).mk fs-img FS_MODE=$(FS_MODE)

run:
	make -f make/$(CURR_ARCH).mk run

gdb:
	make -f make/$(CURR_ARCH).mk gdb

rv64-gdb: rv64-debug
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk comp-gdb

la64-gdb: la64-debug
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk comp-gdb	

rvchange:
	rm -f ../sdcard-rv.img
	cp ../../imgbk/sdcard-rv.img ../sdcard-rv.img

lachange:
	rm -f ../sdcard-la.img
	cp ../../imgbk/sdcard-la.img ../sdcard-la.img

docker:
	docker compose up -d
	docker compose exec -it os-dev bash

conf-inject: prepare-cargo-config
	@arch_list="$(if $(filter file default,$(origin CONF_ARCH)),rv64 la64,$(CONF_ARCH))"; \
	for arch in $$arch_list; do \
		case "$$arch" in \
			la|la64) \
				rustup override set $(LA_TOOLCHAIN); \
				cp ./src/lang_items.rs.la ./src/lang_items.rs; \
				cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs; \
				;; \
			rv|rv64) \
				rustup override set $(RV_TOOLCHAIN); \
				cp ./src/lang_items.rs.rv ./src/lang_items.rs; \
				cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs; \
				;; \
		esac; \
		ARCH=$$arch BLK_MODE=$(CONF_BLK_MODE) CONF_FILE=$(CONF_FILE) IMAGE_PATH=$(CONF_IMAGE) AUTO_REBUILD_MEM=$(AUTO_REBUILD_MEM) MODE=$(MODE) LOG=$(LOG) ./inject_os_test_conf.sh; \
	done

la64-inject-runtime:
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	@cd ../user && make rust-user BOARD=laqemu MODE=$(MODE)
	@cmd_file=$$(mktemp); \
	printf '%s\n' \
		'rm /initproc' \
		'rm /bin/bash' \
		'rm /bin/busybox' \
		'write ../user/target/loongarch64-unknown-linux-gnu/$(MODE)/initproc /initproc' \
		'write ./bash-la /bin/bash' \
		'write ./busybox-la /bin/busybox' \
		'stat /initproc' \
		'stat /bin/bash' \
		'stat /bin/busybox' > $$cmd_file; \
	debugfs -w -f $$cmd_file ../sdcard-la.img; \
	rm -f $$cmd_file

# 将所有用户态测试程序（user/src/bin/*.rs）注入到 sdcard-rv.img
inject-test:
	@rustup override set $(RV_TOOLCHAIN)
	@cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	@cd ../user && make rust-user MODE=release BOARD=rvqemu 2>&1 | tail -5
	@echo "[inject] Updating test binaries in ../sdcard-rv.img..."
	@for testname in $$(ls ../user/src/bin/*.rs | xargs -n1 basename | sed 's/\.rs$$//'); do \
		debugfs -w -R "rm $$testname" ../sdcard-rv.img 2>/dev/null || true; \
		debugfs -w -R "write ../user/target/riscv64gc-unknown-none-elf/release/$$testname $$testname" ../sdcard-rv.img 2>&1; \
	done
	@echo "[inject] Done."
