# Task 3 canonical entrypoint repair evidence

## Tested revision and environment

- Tested revision: `3f2005f3` (`test: recognize shared facade validation`); only the protected baseline paths below were dirty during the acceptance run.
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

## Final clean and root-build contract repair

### RED reproduction

In a Docker dry-run before this repair, both commands exited 0:

```sh
make -C os -n BUILD_ROOT=/tmp/t3-build clean
make -n BUILD_ROOT=/tmp/t3-build COMPAT_OUTPUT_DIR=/tmp/t3-compat clean
```

The OS trace contained only `for arch in rv64; do`, so LA64 clean was omitted. The root trace only delegated to OS clean and removed BUILD_ROOT; it did not remove `kernel-rv`, `kernel-la`, `disk.img`, or `disk-la.img` from COMPAT_OUTPUT_DIR.

### Repair

- `os/Makefile` now explicitly runs both `rv64` and `la64` architecture clean routes before user cleanup.
- Root `Makefile` removes only the four published compatibility artifact names from COMPAT_OUTPUT_DIR and removes BUILD_ROOT; it does not delete arbitrary COMPAT_OUTPUT_DIR content.
- `test-canonical-build-graph.sh` creates temporary BUILD_ROOT/COMPAT_OUTPUT_DIR sentinels, validates both architecture routes, verifies intended artifact removal, and verifies an unrelated sentinel survives.
- `test-root-build-contract.sh` now rejects missing, invalid, and multiple ARCH/PROFILE values before formal build delegation while retaining root `all` evaluator ordering checks.

### Final Docker validation statuses

The final temporary Docker run used the already-recorded provisioned image and same-path linked-worktree/common-Git mounts. It used temporary `safe.directory` configuration only inside the container.

```text
T1-rebaseline-isolation exit=0
T2-source-purity exit=0
T2-rv64-linker-purity exit=0
T2-initramfs-purity exit=0
T2-make-layering exit=0
T2-toolchain-contract exit=0
T2-purity-delta-serial exit=0
facade-root-build exit=0
facade-root-kernel exit=0
facade-root-user exit=0
facade-root-image exit=0
facade-normal-run exit=0
facade-expanded-entrypoint exit=0
T3-canonical-matrix exit=0
T3-second-stage-failure exit=1 (expected 1)
serial-os-clean exit=0
serial-rv64-build exit=0
serial-la64-build exit=0
serial-root-clean exit=0
serial-clean-artifact-sentinels exit=0
```

`serial-clean-artifact-sentinels` proves the temporary BUILD_ROOT and all four known compatibility artifacts were removed, while the unrelated COMPAT_OUTPUT_DIR sentinel remained. The protected dirty snapshot before and after this run remained exactly the eight paths listed above, with no staged entries.

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

## Full acceptance command statuses

The first Compose-container attempt returned `T1-rebaseline-isolation exit=1` because the linked-worktree Git metadata resolves to `/app`, while the Compose service does not mount the common Git directory. A second temporary Docker run with the common Git mount returned the same T1 failure until `safe.directory` was set for `/app` and the actual worktree. These were environment-only setup failures; no T2/T3 command ran in either attempt.

Final validation used temporary image `sha256:636e8abfe817270a7ce84cf9929ec518117b45cf09485138b36b79c8abdc516f`, mounted `/home/pxy/projects/MangoCore-cleanup` read-write and `/home/pxy/projects/MangoCore` read-only at their identical absolute paths. The container used `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`, and temporary `safe.directory` entries for `/app` and `/home/pxy/projects/MangoCore-cleanup`.

```text
T1-rebaseline-isolation exit=0
T2-source-purity exit=0
T2-rv64-linker-purity exit=0
T2-initramfs-purity exit=0
T2-make-layering exit=0
T2-toolchain-contract exit=0
T2-purity-delta-serial exit=0
T3-canonical-matrix exit=0
T3-second-stage-failure exit=1 (expected 1)
facade-root-build exit=0
facade-root-kernel exit=0
facade-root-user exit=0
facade-root-image exit=0
facade-normal-run exit=0
facade-expanded-entrypoint exit=0
ktest-rv64-normal exit=0
ktest-la64-normal exit=0
ktest-rv64-regression exit=0
ktest-la64-regression exit=0
check-rv64-normal exit=0
check-la64-normal exit=0
check-rv64-regression exit=0
check-la64-regression exit=0
```

`T3-second-stage-failure` returning 1 is the required fixture result: it verifies that an LA64 second-stage failure does not publish mixed compatibility artifacts.

## Protected dirty status before and after acceptance

Both snapshots contained exactly the same eight paths and no staged entries:

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
