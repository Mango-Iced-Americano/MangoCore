# T3 formal facade Docker evidence

- Date: 2026-07-22
- Image: `zhouzhouyi/os-contest:20260510`
- Container: Compose service `os-dev`; `/home/pxy/projects/MangoCore-cleanup` is mounted at `/app`.
- Toolchain homes: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`.
- QEMU: not invoked.

## RED

Before the repair, `make -C os -n check` and `make -C os -n rv64-ktest-build-only` accepted omitted formal inputs and exited 0.

## GREEN

```sh
for test_script in scripts/test-root-build-contract.sh scripts/test-root-kernel-contract.sh scripts/test-root-user-contract.sh scripts/test-root-image-contract.sh scripts/test-normal-run-facade-contract.sh scripts/test-canonical-entrypoint-contract.sh; do
  sh "$test_script"
done
```

Exit status: 0. The contracts verify valid delegation plus missing, invalid, and multiple ARCH/PROFILE rejection at the root/OS `check` and `ktest-build-only` boundaries.

```sh
export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
make toolchain-setup
make toolchain-preflight
make -C os rv64-kernel-build-only
make -C os la64-kernel-build-only
```

Exit status: 0. Builds ran serially in RV64 then LA64 order. Existing Rust warnings were emitted; no new compile errors occurred.

## Full acceptance status

Temporary Docker image `sha256:636e8abfe817270a7ce84cf9929ec518117b45cf09485138b36b79c8abdc516f` ran with the linked worktree and common Git directory mounted at their original absolute paths. `safe.directory` was set only in the temporary container for `/app` and the mounted worktree.

```text
T1 rebaseline isolation: 0
T2 source purity/linker purity/initramfs purity/make layering/toolchain/purity delta: 0
T3 serial matrix: 0
T3 second-stage failure fixture: 1 (expected)
six facade and expanded entrypoint contracts: 0
RV64/LA64 x normal/regression ktest-build-only (4 commands): 0
RV64/LA64 x normal/regression formal check (4 commands): 0
```

The protected dirty set was identical before and after: `.gdbinit`, `.omo/boulder.json`, the three listed `cc-codex` deletions, `os_test.conf`, `run_test.sh`, and untracked `docs/Work_Log/2026-07-19.md`. No staged entries were present during validation.
