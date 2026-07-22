# T3 initramfs OUT_DIR Docker evidence

- **Git revision under test:** `40baf0be` (`build: isolate preload feature probes`)
- **Image:** `zhouzhouyi/os-contest:20260510`
- **Container ID:** `053a3e1b7797f2e25e75a1fcb85fa4b4fd7db630f4da04d359560641d13a8983`
- **Mounts:**
  - `/home/pxy/projects/MangoCore` → same path, read-only (linked-worktree common Git metadata)
  - `/home/pxy/projects/MangoCore-cleanup` → same path, read-write (tested worktree)
- **QEMU:** not run by design; this evidence covers build contracts and kernel compilation only.

## Commands and results

1. `make toolchain-setup` and the pinned-toolchain preflight completed in Docker.
2. `sh scripts/test-rebaseline-isolation.sh --allowlist .omo/rebaseline-allowlist.txt --repo-root /home/pxy/projects/MangoCore-cleanup --verify-fingerprints` passed.
3. T2 contracts passed: source purity, RV64 linker source purity, normal initramfs source purity, Make layering, toolchain Make contract, and `test-rebaseline-purity-delta.sh --serial-kernel-builds`.
4. T3 contracts passed: `test-canonical-build-graph.sh --matrix serial`; the `second-stage-failure` fixture emitted its expected `FAIL:` and nonzero exit without publication.
5. Actual Docker build matrix passed serially: RV64/LA64 normal, RV64/LA64 regression, clean→RV64→LA64, clean→LA64→RV64, RV64→RV64, LA64→LA64, RV64 debug→release, and LA64 debug→release.
6. The focused missing-input probe compiled `os/build.rs`, ran it with `CARGO_FEATURE_INITRAMFS=1` and no `MANGO_INITRAMFS_CPIO`, and observed:

   ```text
   MANGO_INITRAMFS_CPIO is required when initramfs is enabled
   PASS: missing initramfs CPIO input fails clearly
   ```

The full matrix command emitted only existing Rust warnings and completed with exit 0. No QEMU output exists because no QEMU target was invoked.
