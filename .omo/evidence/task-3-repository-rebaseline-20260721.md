# Task 3 canonical entrypoint repair evidence

## Tested revision and environment

- Tested worktree: `/home/pxy/projects/MangoCore-cleanup` (dirty while validating this repair).
- Docker image: `zhouzhouyi/os-contest:20260510`.
- Docker development container: `os-dev`, started with `docker compose up -d`; workspace mount `/home/pxy/projects/MangoCore-cleanup` -> `/app` (read-write).
- Rust homes used in the development container: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`.
- QEMU was not invoked. This task changes Makefile facade validation and is verified by dry-run contracts plus compile-only kernel builds.

## Protected pre-existing dirty baseline

The following paths were already dirty and were not modified by this repair:

| Status | Path |
|---|---|
| deleted | `.gdbinit` |
| modified | `.omo/boulder.json` |
| deleted | `cc-codex/comms/2026-06-18-ds-dirty-reclaim-skip-validation.md` |
| deleted | `cc-codex/comms/2026-06-18-ds-incremental-prune-validation.md` |
| deleted | `cc-codex/comms/2026-06-30-comment-refactor-audit.md` |
| modified | `os_test.conf` |
| deleted | `run_test.sh` |
| untracked | `docs/Work_Log/2026-07-19.md` |

## RED reproduction

Before the repair, Docker dry-runs showed that the OS formal `check` and architecture-specific ktest wrappers accepted omitted `ARCH` and/or `PROFILE` through Makefile defaults. The five root facade contracts also failed because their exact-recipe assertions no longer reflected the explicit forwarding interface.

```sh
make -C os -n check
make -C os -n rv64-ktest-build-only
```

Both commands exited 0 before the repair, despite missing formal inputs.

## Repair

- Root and OS formal facades now require explicitly supplied single `ARCH=rv64|la64` and `PROFILE=normal|regression` values before delegation.
- `run`, `user`, and `image` retain their normal-only contract; `test` requires `PROFILE=regression`.
- New root and OS `ktest-build-only` facades accept explicit ARCH/PROFILE and dispatch to the selected architecture implementation. Legacy architecture-named aliases retain their default behavior.
- Contract scripts cover root/OS `check` and `ktest-build-only` missing, invalid, and multiple ARCH/PROFILE inputs.

## GREEN verification

The following Docker contract command exited 0:

```sh
for test_script in \
  scripts/test-root-build-contract.sh \
  scripts/test-root-kernel-contract.sh \
  scripts/test-root-user-contract.sh \
  scripts/test-root-image-contract.sh \
  scripts/test-normal-run-facade-contract.sh \
  scripts/test-canonical-entrypoint-contract.sh; do
  sh "$test_script"
done
```

It verifies valid RV64/LA64 facade delegation, validation-before-delegation, and missing/invalid/multiple input rejection for root and OS `check`/`ktest-build-only`.

The following serial Docker development-container command exited 0 after provisioning the pinned toolchain:

```sh
export RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
make toolchain-setup
make toolchain-preflight
make -C os rv64-kernel-build-only
make -C os la64-kernel-build-only
```

Both builds completed successfully; existing Rust warnings were emitted, with no new compile errors.
