# T6 — ktest process independence and boot-profile legality

- **Date:** 2026-07-22
- **Container:** `c238e449081e4c68d07d193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- **Mount:** `/home/pxy/projects/MangoCore-cleanup -> /app`
- **Revision before commit:** `146d1fa5-dirty`
- **Test configuration:** `os_test.conf` SHA-256 `5d78edc2d7733352046cad727983238de167c597ee6a223afbc980346aa6be22`

## Implementation evidence

- `spawn_ktest_task()` creates a fresh kernel-only PCB through `new_ktest_process()` and registers its TCB/PCB; it no longer dereferences `INITPROC.process`.
- `KTEST_REAPER` is a ktest-only subreaper parent, so ktest child/zombie ownership cannot force lazy construction of normal PID1.
- The independent PCB has a fresh pid/tid and quota guard, a bare address space, empty fd table, fresh `Sighand`/`Futex`, and the initial net/mount/IPC namespaces.
- `boot_block.rs` now owns block discovery and devfs registration. `mount_boot_block_devices()` only applies the temporary compatibility mount policy; T8 remains responsible for moving that policy to PID1.
- `regression_initramfs` and `INITRAMFS_PROFILE_FEATURES` were removed. CPIO profile selection remains exclusively in `MANGO_INITRAMFS_CPIO`.

## Docker verification

Executed serially in the mounted `os-dev` container with
`RUSTUP_HOME=$HOME/.rustup CARGO_HOME=$HOME/.cargo`:

```sh
make -C os rv64-kernel-build-only
make -C os la64-kernel-build-only
make -C os ktest-build-only ARCH=rv64 PROFILE=normal
make -C os ktest-build-only ARCH=la64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal KTEST_FIXTURE=borrows-initproc
```

All commands exited 0. Both RV64 QEMU runs printed `18 passed, 0 failed` and
`[KTEST RESULT: PASS]`. Complete command/QEMU output is retained in:

- `docs/Work_Log/evidence/2026-07-22/t6-rv64-ktest-run.log`
- `docs/Work_Log/evidence/2026-07-22/t6-rv64-ktest-borrows-initproc.log`

The recorded QEMU logs are newer than every changed T6 source/config file.
