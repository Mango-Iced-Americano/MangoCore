# Scoped tools-disk construction.  The caller supplies the final image, payload
# size, source directory, and architecture suffix through build_tools_disk.
define build_tools_disk
	@set -eu; workspace=$$(mktemp -d "$${TMPDIR:-/tmp}/mango-tools-$(4).XXXXXX") || { echo "ERROR: tools workspace creation failed" >&2; exit 1; }; \
	payload="$$workspace/payload.img"; staging="$$workspace/staging"; cleaned=0; \
	cleanup() { status=$$?; [ "$$cleaned" -eq 0 ] || exit "$$status"; cleaned=1; trap - EXIT HUP INT TERM; if [ "$$status" -ne 0 ]; then echo "[tools-disk] failed; preserving $$workspace for diagnostics" >&2; else rm -rf "$$workspace"; fi; exit "$$status"; }; \
	trap cleanup EXIT HUP INT TERM; \
	echo "[tools-disk] Building $(4) tools payload ($(2)MB)..."; \
	mkdir -p "$$staging"; \
	echo "  copying files from $(3)..."; \
	cp -a $(3)/. "$$staging"/ 2>/dev/null || true; \
	echo "  installing persistent /etc config..."; \
	mkdir -p "$$staging/etc"; \
	for f in passwd group hosts resolv.conf nsswitch.conf hostname protocols; do \
		cp -a initramfs/common/etc/"$$f" "$$staging/etc/" 2>/dev/null || true; \
	done; \
	echo "  copying test binaries..."; \
	case "$(4)" in rv) target=riscv64gc-unknown-none-elf ;; la) target=loongarch64-unknown-linux-gnu ;; esac; \
	mkdir -p "$$staging/tests"; \
	for t in inet_test fs_test unix_test; do \
		src="../user/target/$$target/release/$$t"; \
		if [ -f "$$src" ]; then cp -a "$$src" "$$staging/tests/"; echo "    $$t"; \
		else echo "    [skip] $$t (not built)"; fi; \
	done; \
	echo "  copying CPython runtime/tests..."; \
	if [ -d $(CPYTHON_COMMON) ]; then \
		mkdir -p "$$staging/tests/cpython"; \
		cp -a $(CPYTHON_COMMON)/. "$$staging/tests/cpython/"; \
		echo "    [cpython] common scripts included"; \
	else echo "    [cpython] common scripts missing, skipping"; fi; \
	if [ -d $(3)/tests/cpython ]; then \
		mkdir -p "$$staging/tests/cpython"; \
		cp -a $(3)/tests/cpython/. "$$staging/tests/cpython/"; \
		if [ -x "$$staging/tests/cpython/usr/bin/python3" ]; then echo "    [cpython] runtime included"; \
		else echo "    [cpython] arch runtime copied but python3 not found"; fi; \
	else echo "    [cpython] no arch runtime cache, skipping"; fi; \
	if [ -d $(3)/apk ]; then cp -a $(3)/apk "$$staging/" 2>/dev/null; echo "  [apk] local repo included"; fi; \
	echo "  creating symlinks ..."; \
	cd "$$staging/lib" && \
		if [ -f libc.so ]; then \
			ln -sf libc.so ld-musl-riscv64.so.1 2>/dev/null; \
			ln -sf libc.so ld-musl-riscv64-sf.so.1 2>/dev/null; \
			ln -sf libc.so ld-musl-loongarch-lp64d.so.1 2>/dev/null; \
			ln -sf libc.so libc.musl-riscv64.so.1 2>/dev/null; \
			ln -sf libc.so libc.musl-loongarch64.so.1 2>/dev/null; \
		fi; \
	cd "$$staging/lib" && \
		for f in *.so*; do \
			case "$$f" in \
				libc.so|libc.so.6|libm.so.6|ld-linux-riscv64-lp64d.so.1|ld-linux-loongarch-lp64d.so.1|libgcc_s.so.1) continue ;; \
				*) ln -sf "$$f" "lib/$$f" 2>/dev/null || true ;; \
			esac; \
		done 2>/dev/null; true; \
	echo "  pre-installing critical busybox applets ..."; \
	cd "$$staging/bin" && \
		for applet in cp mv rm ln ls mkdir chmod cat printf sleep grep sed awk uname basename dirname true false test mkfs.vfat; do \
			ln -sf busybox "$$applet" 2>/dev/null; \
		done && ln -sf bash sh 2>/dev/null || true; \
	dd if=/dev/zero of="$$payload" bs=1M count=$(2) 2>/dev/null; \
	mke2fs -t ext4 -F -b 4096 -d "$$staging" "$$payload" 2>/dev/null; \
	echo "[tools-disk] Wrapping with MBR → $(1)..."; \
	mkdir -p "$(dir $(1))"; \
	python3 "$(abspath $(MBR_SCRIPT))" "$$payload" "$(1)"; \
	rm -f "$$payload"; trap - EXIT HUP INT TERM; rm -rf "$$workspace"; \
	echo "[tools-disk] $(1) ready."
endef
