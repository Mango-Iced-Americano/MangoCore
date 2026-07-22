# T6 ktest Docker/QEMU evidence

- Container: `c238e449081e4c68d07d193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`
- Config: `os_test.conf` SHA-256 `5d78edc2d7733352046cad727983238de167c597ee6a223afbc980346aa6be22`

The final serial Docker matrix exited 0:

```sh
make -C os rv64-kernel-build-only
make -C os la64-kernel-build-only
make -C os ktest-build-only ARCH=rv64 PROFILE=normal
make -C os ktest-build-only ARCH=la64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal KTEST_FIXTURE=borrows-initproc
```

Each RV64 QEMU smoke log reports `18 passed, 0 failed` and `[KTEST RESULT: PASS]`:

- `t6-rv64-ktest-run.log`
- `t6-rv64-ktest-borrows-initproc.log`

The fixture command is deliberately run after ktest no longer references
`INITPROC.process`; the normal PID1 ELF remains unconstructed on the ktest path.
