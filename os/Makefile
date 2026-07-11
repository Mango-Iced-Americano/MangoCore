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

.PHONY: all $(ARCHS) clean prepare-cargo-config \
	la64-2k1000-sata-probe la64-2k1000-sata-mount-ro \
	la64-2k1000-run-clean

all: prepare-cargo-config rv64_all la64_all

prepare-cargo-config:
	@sh ../scripts/restore-cargo-vendor-checksums.sh restore ..
	@mkdir -p .cargo ../user/.cargo
	@test -f .cargo/config.toml || cp -f ../cargo-config/os/config.toml .cargo/config.toml
	@test -f ../user/.cargo/config.toml || cp -f ../cargo-config/user/config.toml ../user/.cargo/config.toml

rv64_all: prepare-cargo-config tools-disk-rv
	@echo "Building rv64"
	@rustup override set $(RV_TOOLCHAIN)
	cp ./src/lang_items.rs.rv ./src/lang_items.rs
	cp ../user/src/lang_items.rs.rv ../user/src/lang_items.rs
	make ARCH=rv64 -f make/rv64.mk all MODE=${MODE}

la64_all: prepare-cargo-config tools-disk-la
	@echo "Building la64"
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make ARCH=la64 -f make/la64.mk all MODE=${MODE} LOG=${LOG}

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
	make -f make/la64.mk all MODE=${MODE} LOG=${LOG}

la64-kernel-build-only: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64.mk build BLK_MODE=${LA64_BLK_MODE} MODE=${MODE} LOG=${LOG}

# Read-only real-board image: IDENTIFY DEVICE and two LBA0 reads, then ramfs shell.
la64-2k1000-sata-probe: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	$(MAKE) -B -f make/la64.mk uimage BOARD=2k1000 BLK_MODE=sata \
		EXTRA_FEATURES=sata_probe MODE=$(MODE) LOG=off
	cp -f target/loongarch64-unknown-linux-gnu/$(MODE)/os.ui \
		../kernel-2k1000-sata-probe.ui

# 首次文件系统验收镜像：识别裸盘/MBR 中的 ext4 或 FAT32，并以只读方式挂载。
la64-2k1000-sata-mount-ro: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	$(MAKE) -B -f make/la64.mk uimage BOARD=2k1000 BLK_MODE=sata \
		MODE=$(MODE) LOG=off
	cp -f target/loongarch64-unknown-linux-gnu/$(MODE)/os.ui \
		../kernel-2k1000-sata-mount-ro.ui

# Competition run image: no board bring-up trace, log level off, and the
# embedded fallback configuration must be the normal full-suite run mode.
la64-2k1000-run-clean: prepare-cargo-config
	@grep -Eq '^mode=run$$' ../os_test.conf || { \
		echo "os_test.conf must contain mode=run" >&2; exit 1; \
	}
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	$(MAKE) -B -f make/la64.mk uimage BOARD=2k1000 BLK_MODE=sata \
		MODE=$(MODE) LOG=off
	cp -f target/loongarch64-unknown-linux-gnu/$(MODE)/os.ui \
		../kernel-2k1000-run.ui

la64-debug:
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64.mk all MODE=debug	

la64-run-only:
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64.mk runsimple MODE=${MODE} LOG=${LOG}

la64-run: prepare-cargo-config
	@rustup override set $(LA_TOOLCHAIN)
	cp ./src/lang_items.rs.la ./src/lang_items.rs
	cp ../user/src/lang_items.rs.la ../user/src/lang_items.rs
	make -f make/la64.mk comp MODE=${MODE} LOG=${LOG}

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
	make -f make/la64.mk comp-gdb	

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

# ============================================================
# 工具盘构建 (disk.img / disk-la.img)
# 内容：busybox + bash + musl/glibc 库 + 软链接
# 来源：user/tools/{riscv64,loongarch64}/
# ============================================================

TOOLS_IMG_RV := ../disk.img
TOOLS_IMG_LA := ../disk-la.img
TOOLS_SIZE_RV := 768
TOOLS_SIZE_LA := 768
TOOLS_SRC_RV := ../user/tools/riscv64
TOOLS_SRC_LA := ../user/tools/loongarch64

# MBR 脚本路径
MBR_SCRIPT := ../scripts/make_mbr_tools_disk.py

# 构建 tools 目录的通用配方
# $(1)=最终盘镜像路径 $(2)=payload 大小MB $(3)=源目录 $(4)=架构后缀(rv|la)
define build_tools_disk
	@echo "[tools-disk] Building $(4) tools payload ($(2)MB)..."
	@dd if=/dev/zero of=/tmp/tools-pl-$(4).img bs=1M count=$(2) 2>/dev/null
	@mkfs.ext4 -F /tmp/tools-pl-$(4).img -b 4096 2>/dev/null
	@mkdir -p /tmp/tools-mnt
	@mount -o loop /tmp/tools-pl-$(4).img /tmp/tools-mnt
	@mkdir -p /tmp/tools-mnt/bin /tmp/tools-mnt/lib /tmp/tools-mnt/usr/bin /tmp/tools-mnt/usr/sbin /tmp/tools-mnt/etc /tmp/tools-mnt/tests
	@echo "  copying files..."
	@cp -a $(3)/bin/* /tmp/tools-mnt/bin/ 2>/dev/null || true
	@cp -a $(3)/lib/* /tmp/tools-mnt/lib/ 2>/dev/null || true
	@cp -a $(3)/sbin/* /tmp/tools-mnt/sbin/ 2>/dev/null || true
	@cp -a $(3)/etc/* /tmp/tools-mnt/etc/ 2>/dev/null || true
	@echo "  copying test binaries..."
	@case "$(4)" in rv) target=riscv64gc-unknown-none-elf ;; la) target=loongarch64-unknown-linux-gnu ;; esac; \
	for t in inet_test fs_test unix_test; do \
		src="../user/target/$$target/release/$$t"; \
		if [ -f "$$src" ]; then cp -a "$$src" /tmp/tools-mnt/tests/; echo "    $$t"; \
		else echo "    [skip] $$t (not built)"; fi; \
	done
	@if [ -d $(3)/apk ]; then \
		cp -a $(3)/apk /tmp/tools-mnt/ 2>/dev/null; \
		echo "  [apk] local repo included"; \
	fi
	@echo "  creating symlinks ..."
	@cd /tmp/tools-mnt/lib && \
		if [ -f libc.so ]; then \
			ln -sf libc.so ld-musl-riscv64.so.1 2>/dev/null; \
			ln -sf libc.so ld-musl-riscv64-sf.so.1 2>/dev/null; \
			ln -sf libc.so ld-musl-loongarch-lp64d.so.1 2>/dev/null; \
			ln -sf libc.so libc.musl-riscv64.so.1 2>/dev/null; \
			ln -sf libc.so libc.musl-loongarch64.so.1 2>/dev/null; \
		fi
	@cd /tmp/tools-mnt/lib && \
		for f in *.so*; do \
			[ "$$f" = "libc.so" ] && continue; \
			[ "$$f" = "libc.so.6" ] && continue; \
			[ "$$f" = "libm.so.6" ] && continue; \
			[ "$$f" = "ld-linux-riscv64-lp64d.so.1" ] && continue; \
			[ "$$f" = "ld-linux-loongarch-lp64d.so.1" ] && continue; \
			[ "$$f" = "libgcc_s.so.1" ] && continue; \
			ln -sf "$$f" "lib/$$f" 2>/dev/null; \
		done 2>/dev/null; true
	@echo "  pre-installing critical busybox applets ..."
	@cd /tmp/tools-mnt/bin && \
		for applet in cp mv rm ln ls mkdir chmod cat printf sleep grep sed awk uname basename dirname true false test; do \
			ln -sf busybox $$applet 2>/dev/null; \
		done
	@cd /tmp/tools-mnt/bin && ln -sf bash sh 2>/dev/null || true
	@# mkfs applets for LTP device tests
	@cd /tmp/tools-mnt/bin && \
		for applet in mkfs.vfat; do \
			ln -sf busybox $$applet 2>/dev/null; \
		done
	@umount /tmp/tools-mnt
	@rmdir /tmp/tools-mnt
	@echo "[tools-disk] Wrapping with MBR → $(1)..."
	@python3 $(MBR_SCRIPT) /tmp/tools-pl-$(4).img $(1)
	@rm -f /tmp/tools-pl-$(4).img
	@echo "[tools-disk] $(1) ready."
endef

tools-disk-rv:
	$(call build_tools_disk,$(TOOLS_IMG_RV),$(TOOLS_SIZE_RV),$(TOOLS_SRC_RV),rv)

tools-disk-la:
	$(call build_tools_disk,$(TOOLS_IMG_LA),$(TOOLS_SIZE_LA),$(TOOLS_SRC_LA),la)

tools-disk: tools-disk-rv tools-disk-la

# ============================================================
# Alpine 预编译工具下载（本地缓存，避免每次下载）
# 下载到 user/tools/{arch}/，tools-disk 自动打包
# ============================================================

ALPINE_MIRROR := https://dl-cdn.alpinelinux.org/alpine/edge/main

tools-alpine-rv:
	@echo "[alpine] Downloading riscv64 tools..."
	@mkdir -p /tmp/alpine-rv
	@for pkg_info in \
		"tcpdump:usr/bin/tcpdump:tcpdump:bin" \
		"iproute2-minimal:sbin/ip:ip:bin" \
		"iproute2-ss:sbin/ss:ss:bin" \
		"libpcap:usr/lib/libpcap.so.1:libpcap.so.1:lib" \
		"libcrypto3:usr/lib/libcrypto.so.3:libcrypto.so.3:lib" \
		"libcap2:usr/lib/libcap.so.2:libcap.so.2:lib" \
		"libmnl:usr/lib/libmnl.so.0:libmnl.so.0:lib" \
		"libelf:usr/lib/libelf.so.1:libelf.so.1:lib"; do \
		pkg=$${pkg_info%%:*}; rest1=$${pkg_info#*:}; apath=$${rest1%%:*}; rest2=$${rest1#*:}; oname=$${rest2%%:*}; subdir=$${pkg_info##*:}; \
		dest=$(CURDIR)/../user/tools/riscv64/$$subdir/$$oname; \
		if [ -f "$$dest" ]; then \
			echo "  [cache] $$oname"; continue; \
		fi; \
		apk=$$(curl -sL "$(ALPINE_MIRROR)/riscv64/" | grep -oP "\"$$pkg-[0-9][^\"]*\.apk\"" | tr -d '"' | sort -V | tail -1); \
		if [ -z "$$apk" ]; then echo "  [skip] $$pkg not found"; continue; fi; \
		if [ ! -f "/tmp/alpine-rv/$$apk" ]; then \
			echo "  fetching $$apk..."; \
			curl -sL "$(ALPINE_MIRROR)/riscv64/$$apk" -o "/tmp/alpine-rv/$$apk"; \
		fi; \
		tar -xzf "/tmp/alpine-rv/$$apk" -C /tmp/alpine-rv 2>/dev/null; \
		cp /tmp/alpine-rv/$$apath "$$dest" 2>/dev/null && echo "  [done] $$oname"; \
	done
	@rm -rf /tmp/alpine-rv
	@echo "[alpine] riscv64 done."

tools-alpine-la:
	@echo "[alpine] Downloading loongarch64 tools..."
	@mkdir -p /tmp/alpine-la
	@for pkg_info in \
		"tcpdump:usr/bin/tcpdump:tcpdump:bin" \
		"iproute2-minimal:sbin/ip:ip:bin" \
		"iproute2-ss:sbin/ss:ss:bin" \
		"libpcap:usr/lib/libpcap.so.1:libpcap.so.1:lib" \
		"libcrypto3:usr/lib/libcrypto.so.3:libcrypto.so.3:lib" \
		"libcap2:usr/lib/libcap.so.2:libcap.so.2:lib" \
		"libmnl:usr/lib/libmnl.so.0:libmnl.so.0:lib" \
		"libelf:usr/lib/libelf.so.1:libelf.so.1:lib"; do \
		pkg=$${pkg_info%%:*}; rest1=$${pkg_info#*:}; apath=$${rest1%%:*}; rest2=$${rest1#*:}; oname=$${rest2%%:*}; subdir=$${pkg_info##*:}; \
		dest=$(CURDIR)/../user/tools/loongarch64/$$subdir/$$oname; \
		if [ -f "$$dest" ]; then \
			echo "  [cache] $$oname"; continue; \
		fi; \
		apk=$$(curl -sL "$(ALPINE_MIRROR)/loongarch64/" | grep -oP "\"$$pkg-[0-9][^\"]*\.apk\"" | tr -d '"' | sort -V | tail -1); \
		if [ -z "$$apk" ]; then echo "  [skip] $$pkg not found"; continue; fi; \
		if [ ! -f "/tmp/alpine-la/$$apk" ]; then \
			echo "  fetching $$apk..."; \
			curl -sL "$(ALPINE_MIRROR)/loongarch64/$$apk" -o "/tmp/alpine-la/$$apk"; \
		fi; \
		tar -xzf "/tmp/alpine-la/$$apk" -C /tmp/alpine-la 2>/dev/null; \
		cp /tmp/alpine-la/$$apath "$$dest" 2>/dev/null && echo "  [done] $$oname"; \
	done
	@rm -rf /tmp/alpine-la
	@echo "[alpine] loongarch64 done."

tools-alpine: tools-alpine-rv tools-alpine-la

# ============================================================
# apk 包管理器下载 + 本地 repo 配置
# 下载 apk-tools-static, alpine-keys, 示例包
# ============================================================

APK_PACKAGES := musl zlib ncurses

tools-apk-rv:
	@echo "[apk] Setting up apk for riscv64..."
	@mkdir -p /tmp/apk-rv /tmp/apk-rv-keys
	# 1. 下载 apk-tools-static → bin/apk.static
	@apk_url="$(ALPINE_MIRROR)/riscv64/"; \
		apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP '"apk-tools-static-[0-9][^"]*\.apk"' | tr -d '"' | sort -V | tail -1); \
		if [ -n "$$apk_file" ]; then \
			dest=$(CURDIR)/../user/tools/riscv64/bin/apk.static; \
			if [ ! -f "$$dest" ]; then \
				echo "  fetching $$apk_file..."; \
				curl -sL "$$apk_url/$$apk_file" -o /tmp/apk-rv/$$apk_file && \
				tar -xzf /tmp/apk-rv/$$apk_file -C /tmp/apk-rv 2>/dev/null && \
				cp /tmp/apk-rv/sbin/apk.static "$$dest" 2>/dev/null && \
				echo "  [done] apk.static"; \
			else echo "  [cache] apk.static"; fi; \
		else echo "  [skip] apk-tools-static not found"; fi
	# 2. 下载 alpine-keys → etc/apk/keys/
	@apk_url="$(ALPINE_MIRROR)/riscv64/"; \
		apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP '"alpine-keys-[0-9][^"]*\.apk"' | tr -d '"' | sort -V | tail -1); \
		if [ -n "$$apk_file" ]; then \
			keydir=$(CURDIR)/../user/tools/riscv64/etc/apk/keys; \
			mkdir -p "$$keydir"; \
			if [ -z "$$(ls -A "$$keydir" 2>/dev/null)" ]; then \
				echo "  fetching $$apk_file..."; \
				curl -sL "$$apk_url/$$apk_file" -o /tmp/apk-rv/$$apk_file && \
				tar -xzf /tmp/apk-rv/$$apk_file -C /tmp/apk-rv-keys 2>/dev/null && \
				cp /tmp/apk-rv-keys/etc/apk/keys/*.pub "$$keydir/" 2>/dev/null && \
				echo "  [done] alpine keys"; \
			else echo "  [cache] alpine keys"; fi; \
		else echo "  [skip] alpine-keys not found"; fi
	# 3. 创建 /etc/apk/repositories (指向官方源 + 本地包)
	@repodir=$(CURDIR)/../user/tools/riscv64/etc/apk; \
		mkdir -p "$$repodir"; \
		if [ ! -f "$$repodir/repositories" ]; then \
			printf '%s\n' \
				"https://dl-cdn.alpinelinux.org/alpine/edge/main" \
				"file:///tools/apk/packages" > "$$repodir/repositories"; \
			echo "  [done] etc/apk/repositories"; \
		else echo "  [cache] etc/apk/repositories"; fi
	# 4. 下载示例包 + APKINDEX → apk/packages/<arch>/
	@pkgdir=$(CURDIR)/../user/tools/riscv64/apk/packages/riscv64; \
		mkdir -p "$$pkgdir"; \
		if [ ! -f "$$pkgdir/APKINDEX.tar.gz" ]; then \
			echo "  fetching APKINDEX.tar.gz..."; \
			curl -sL "$(ALPINE_MIRROR)/riscv64/APKINDEX.tar.gz" -o "$$pkgdir/APKINDEX.tar.gz" 2>/dev/null && \
			echo "  [done] APKINDEX.tar.gz"; \
		else echo "  [cache] APKINDEX.tar.gz"; fi; \
		apk_url="$(ALPINE_MIRROR)/riscv64/"; \
		for pkg in $(APK_PACKAGES); do \
			apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP "\"$$pkg-[0-9][^\"]*\.apk\"" | tr -d '"' | sort -V | tail -1); \
			if [ -n "$$apk_file" ]; then \
				if [ ! -f "$$pkgdir/$$apk_file" ]; then \
					echo "  fetching $$apk_file..."; \
					curl -sL "$$apk_url/$$apk_file" -o "$$pkgdir/$$apk_file" 2>/dev/null && \
					echo "  [done] $$apk_file"; \
				else echo "  [cache] $$apk_file"; fi; \
			else echo "  [skip] $$pkg not found in repo"; fi; \
		done
	@rm -rf /tmp/apk-rv /tmp/apk-rv-keys
	@echo "[apk] riscv64 done."

tools-apk-la:
	@echo "[apk] Setting up apk for loongarch64..."
	@mkdir -p /tmp/apk-la /tmp/apk-la-keys
	# 1. apk-tools-static → bin/apk.static
	@apk_url="$(ALPINE_MIRROR)/loongarch64/"; \
		apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP '"apk-tools-static-[0-9][^"]*\.apk"' | tr -d '"' | sort -V | tail -1); \
		if [ -n "$$apk_file" ]; then \
			dest=$(CURDIR)/../user/tools/loongarch64/bin/apk.static; \
			if [ ! -f "$$dest" ]; then \
				echo "  fetching $$apk_file..."; \
				curl -sL "$$apk_url/$$apk_file" -o /tmp/apk-la/$$apk_file && \
				tar -xzf /tmp/apk-la/$$apk_file -C /tmp/apk-la 2>/dev/null && \
				cp /tmp/apk-la/sbin/apk.static "$$dest" 2>/dev/null && \
				echo "  [done] apk.static"; \
			else echo "  [cache] apk.static"; fi; \
		else echo "  [skip] apk-tools-static not found"; fi
	# 2. alpine-keys → etc/apk/keys/
	@apk_url="$(ALPINE_MIRROR)/loongarch64/"; \
		apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP '"alpine-keys-[0-9][^"]*\.apk"' | tr -d '"' | sort -V | tail -1); \
		if [ -n "$$apk_file" ]; then \
			keydir=$(CURDIR)/../user/tools/loongarch64/etc/apk/keys; \
			mkdir -p "$$keydir"; \
			if [ -z "$$(ls -A "$$keydir" 2>/dev/null)" ]; then \
				echo "  fetching $$apk_file..."; \
				curl -sL "$$apk_url/$$apk_file" -o /tmp/apk-la/$$apk_file && \
				tar -xzf /tmp/apk-la/$$apk_file -C /tmp/apk-la-keys 2>/dev/null && \
				cp /tmp/apk-la-keys/etc/apk/keys/*.pub "$$keydir/" 2>/dev/null && \
				echo "  [done] alpine keys"; \
			else echo "  [cache] alpine keys"; fi; \
		else echo "  [skip] alpine-keys not found"; fi
	# 3. 创建 /etc/apk/repositories
	@repodir=$(CURDIR)/../user/tools/loongarch64/etc/apk; \
		mkdir -p "$$repodir"; \
		if [ ! -f "$$repodir/repositories" ]; then \
			printf '%s\n' \
				"https://dl-cdn.alpinelinux.org/alpine/edge/main" \
				"file:///tools/apk/packages" > "$$repodir/repositories"; \
			echo "  [done] etc/apk/repositories"; \
		else echo "  [cache] etc/apk/repositories"; fi
	# 4. 下载示例包 + APKINDEX → apk/packages/<arch>/
	@pkgdir=$(CURDIR)/../user/tools/loongarch64/apk/packages/loongarch64; \
		mkdir -p "$$pkgdir"; \
		if [ ! -f "$$pkgdir/APKINDEX.tar.gz" ]; then \
			echo "  fetching APKINDEX.tar.gz..."; \
			curl -sL "$(ALPINE_MIRROR)/loongarch64/APKINDEX.tar.gz" -o "$$pkgdir/APKINDEX.tar.gz" 2>/dev/null && \
			echo "  [done] APKINDEX.tar.gz"; \
		else echo "  [cache] APKINDEX.tar.gz"; fi; \
		apk_url="$(ALPINE_MIRROR)/loongarch64/"; \
		for pkg in $(APK_PACKAGES); do \
			apk_file=$$(curl -sL "$$apk_url" 2>/dev/null | grep -oP "\"$$pkg-[0-9][^\"]*\.apk\"" | tr -d '"' | sort -V | tail -1); \
			if [ -n "$$apk_file" ]; then \
				if [ ! -f "$$pkgdir/$$apk_file" ]; then \
					echo "  fetching $$apk_file..."; \
					curl -sL "$$apk_url/$$apk_file" -o "$$pkgdir/$$apk_file" 2>/dev/null && \
					echo "  [done] $$apk_file"; \
				else echo "  [cache] $$apk_file"; fi; \
			else echo "  [skip] $$pkg not found in repo"; fi; \
		done
	@rm -rf /tmp/apk-la /tmp/apk-la-keys
	@echo "[apk] loongarch64 done."

tools-apk: tools-apk-rv tools-apk-la

# ============================================================
# Initramfs 构建
# 生成 tiny newc cpio 归档，嵌入 kernel 作为初始根文件系统
# ============================================================

INITRAMFS_DIR_RV := ../fs-img-dir/initramfs-rv.cpio
INITRAMFS_DIR_LA := ../fs-img-dir/initramfs-la.cpio

# DNS 服务器：QEMU user 网用 10.0.2.3，真机/其他环境可覆写
DNS_SERVER ?= 10.0.2.3

initramfs-rv: user
	@DNS_SERVER=$(DNS_SERVER) ./build_initramfs.sh rv64 $(MODE) $(INITRAMFS_DIR_RV)

initramfs-la: user
	@DNS_SERVER=$(DNS_SERVER) ./build_initramfs.sh la64 $(MODE) $(INITRAMFS_DIR_LA)

initramfs-all: initramfs-rv initramfs-la
