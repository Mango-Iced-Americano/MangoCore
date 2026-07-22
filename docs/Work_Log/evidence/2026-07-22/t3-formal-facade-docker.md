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
