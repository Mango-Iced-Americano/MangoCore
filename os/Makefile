ARCHS = rv64
CURR_ARCH ?= la64
FS_MODE ?= ext4
BLK_MODE ?= virt
MODE ?= release
CONF_FILE ?= ../os_test.conf
CONF_ARCH ?= $(CURR_ARCH)
CONF_BLK_MODE ?= $(BLK_MODE)
CONF_IMAGE ?=
AUTO_REBUILD_MEM ?= 1

.PHONY: all $(ARCHS) clean

all: rv64_all la64_all

rv64_all:
	@echo "Building rv64"
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make ARCH=rv64 -f make/rv64.mk all MODE=${MODE}

la64_all:
	@echo "Building la64"
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make ARCH=la64 -f make/la64o.mk all MODE=${MODE}

rv64-kernel-build-only:
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk build BLK_MODE=${BLK_MODE} MODE=${MODE} LOG=${LOG}

rv64-debug:
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk all BLK_MODE=${BLK_MODE} MODE=debug

rv64-only:
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk all BLK_MODE=${BLK_MODE}

rv64-run-only:
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk runsimple

rv64-run:
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk comp

la64-only:
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk all 

la64-kernel-build-only:
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk build MODE=${MODE} LOG=${LOG}

la64-debug:
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk all MODE=debug	

la64-run-only:
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk runsimple

la64-run:
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64o.mk comp

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
	@rustup override set nightly-2025-01-18-x86_64-unknown-linux-gnu
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make -f make/rv64.mk comp-gdb

la64-gdb: la64-debug
	@rustup override set nightly-2024-05-01-x86_64-unknown-linux-gnu
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

conf-inject:
	ARCH=$(CONF_ARCH) BLK_MODE=$(CONF_BLK_MODE) CONF_FILE=$(CONF_FILE) IMAGE_PATH=$(CONF_IMAGE) AUTO_REBUILD_MEM=$(AUTO_REBUILD_MEM) MODE=$(MODE) LOG=$(LOG) ./inject_os_test_conf.sh