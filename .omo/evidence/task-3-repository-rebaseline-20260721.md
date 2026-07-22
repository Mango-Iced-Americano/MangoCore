# Task 3 canonical entrypoint repair evidence

## Tested revision and environment

- Tested code revision: `c66eb3af` (`build: preserve user artifact mode for checks`); worktree was dirty only because the pre-existing protected `os_test.conf` remained uncommitted.
- Docker image: `zhouzhouyi/os-contest:20260510`.
- Worktree mount: `/home/pxy/projects/MangoCore-cleanup` → same container path, read-write.
- Linked-worktree common Git mount: `/home/pxy/projects/MangoCore` → same container path, read-only.
- `os_test.conf` SHA-256: `5d78edc2d7733352046cad727983238de167c597ee6a223afbc980346aa6be22`.
- QEMU: not invoked. `ktest-build-only` was used for compile-only verification.

## Reproduction

Before repair, Docker dry-runs showed that `ktest-run` Cargo commands contained neither `MANGO_INITRAMFS_CPIO` nor `MANGO_USER_OUTPUT_ROOT`; formal `check` invoked bare Cargo without a board feature or artifact inputs.

## Repair proof

The expanded contract passes all RV64/LA64 × normal/regression paths:

```sh
sh scripts/test-canonical-entrypoint-contract.sh
```

It verifies that `ktest-build-only`, inherited `ktest-run`, and formal `check` use absolute CPIO/user-root/user-mode inputs; formal checks use `board_rvqemu`/`board_laqemu` and explicit targets; invalid ARCH and PROFILE are rejected.

## Full Docker validation

The following single serial Docker command exited 0:

```sh
make toolchain-setup
sh scripts/test-rebaseline-isolation.sh --allowlist .omo/rebaseline-allowlist.txt --repo-root /home/pxy/projects/MangoCore-cleanup --verify-fingerprints
sh scripts/test-source-purity-make-contract.sh
sh scripts/test-normal-rv64-linker-source-purity-contract.sh
sh scripts/test-normal-initramfs-source-purity-contract.sh
sh scripts/test-make-layering-contract.sh
sh scripts/test-toolchain-make-contract.sh
sh scripts/test-rebaseline-purity-delta.sh --serial-kernel-builds
sh scripts/test-canonical-build-graph.sh --matrix serial
sh scripts/test-canonical-entrypoint-contract.sh
make -C os PROFILE=normal rv64-ktest-build-only
make -C os PROFILE=normal la64-ktest-build-only
make -C os PROFILE=regression rv64-ktest-build-only
make -C os PROFILE=regression la64-ktest-build-only
make -C os ARCH=rv64 PROFILE=normal check
make -C os ARCH=la64 PROFILE=normal check
make -C os ARCH=rv64 PROFILE=regression check
make -C os ARCH=la64 PROFILE=regression check
```

The canonical second-stage-failure fixture was also run in the same command and produced its expected nonzero `FAIL:` result without publishing compatibility artifacts. Existing Rust warnings were emitted; no new compile errors occurred.
