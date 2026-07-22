include make/tools-disk.mk

tools-disk-rv: maybe-tools-cpython-rv
	$(call build_tools_disk,$(TOOLS_IMG_RV),$(TOOLS_SIZE_RV),$(TOOLS_SRC_RV),rv)

tools-disk-la: maybe-tools-cpython-la
	$(call build_tools_disk,$(TOOLS_IMG_LA),$(TOOLS_SIZE_LA),$(TOOLS_SRC_LA),la)

tools-disk: tools-disk-rv tools-disk-la

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
# CPython runtime downloader（从 Alpine 下载 python3 及依赖）
# 缓存到 user/tools/{arch}/tests/cpython/（gitignored）
# ============================================================

tools-cpython-rv:
	@python3 ../scripts/fetch_cpython_runtime.py \
		--arch riscv64 \
		--mirror $(ALPINE_MIRROR) \
		--dest ../user/tools/riscv64/tests/cpython

tools-cpython-la:
	@python3 ../scripts/fetch_cpython_runtime.py \
		--arch loongarch64 \
		--mirror $(ALPINE_MIRROR) \
		--dest ../user/tools/loongarch64/tests/cpython

tools-cpython: tools-cpython-rv tools-cpython-la

maybe-tools-cpython-rv:
	@if [ "$(CPYTHON_AUTO)" = "1" ]; then \
		$(MAKE) --no-print-directory tools-cpython-rv || \
		echo "[cpython] riscv64 download failed, continuing without CPython runtime"; \
	else \
		echo "[cpython] CPYTHON_AUTO=0, skipping riscv64 runtime"; \
	fi

maybe-tools-cpython-la:
	@if [ "$(CPYTHON_AUTO)" = "1" ]; then \
		$(MAKE) --no-print-directory tools-cpython-la || \
		echo "[cpython] loongarch64 download failed, continuing without CPython runtime"; \
	else \
		echo "[cpython] CPYTHON_AUTO=0, skipping loongarch64 runtime"; \
	fi

tools-cpython-clean:
	@rm -rf $(CPYTHON_SRC_RV)/.apk-cache $(CPYTHON_SRC_RV)/lib $(CPYTHON_SRC_RV)/usr $(CPYTHON_SRC_RV)/etc $(CPYTHON_SRC_RV)/var $(CPYTHON_SRC_RV)/.cpython-*.stamp $(CPYTHON_SRC_RV)/manifest*.txt
	@rm -rf $(CPYTHON_SRC_LA)/.apk-cache $(CPYTHON_SRC_LA)/lib $(CPYTHON_SRC_LA)/usr $(CPYTHON_SRC_LA)/etc $(CPYTHON_SRC_LA)/var $(CPYTHON_SRC_LA)/.cpython-*.stamp $(CPYTHON_SRC_LA)/manifest*.txt
	@echo "[cpython] cache cleaned"

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
