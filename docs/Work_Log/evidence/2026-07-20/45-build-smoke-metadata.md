# Detached build commit QEMU regression smoke — 2026-07-20

- Tested commit: `504c605a test: add la64 regression log gate`
- Detached host snapshot: `/tmp/opencode/mangocore-qemu-smoke`
- Docker container: `c238e449081e4c68d07b193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- Source snapshot execution directory: `/tmp/mangocore-qemu-smoke`
- Toolchain environment: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`
- Host worktree mount: `/home/pxy/projects/MangoCore-cleanup -> /app`; detached snapshot was copied into container-local `/tmp` so uncommitted runtime files could not affect the result.

## Commands and order

1. `make -C os toolchain-preflight`
2. `make -C os rv64-regression MODE=release LOG=error`
3. `make -C os toolchain-preflight`
4. `make -C os la64-regression MODE=release LOG=error`

Both architecture builds completed before their QEMU run. The runs were serial.

## Configuration

No `os_test.conf` was injected: standard basic sdcard images were absent locally, so this is a diskless regression-initramfs smoke rather than the normal/basic evaluator suite.

## Outcomes

- RV64 command exited nonzero after QEMU reported `4 passed, 1 failed, 5 total`; the failing fixture expects an unaligned `mprotect` call to succeed despite Linux page-alignment semantics.
- LA64 command exited nonzero after a pre-PID1 `map_elf` `AlreadyMapped` panic. The committed log classifier ran and emitted `STATE=ENTRY_FAILURE STATUS=0`.
- Neither result is a QEMU PASS. The full console logs and head/tail summaries are adjacent evidence files.
