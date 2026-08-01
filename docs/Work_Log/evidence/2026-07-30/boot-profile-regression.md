# Boot Profile and FDT Runtime Discovery Verification

> **Archive path:** `docs/Work_Log/evidence/2026-07-30/boot-profile-regression.md`
> **Generated:** 2026-07-30
> **Parent work log:** `docs/Work_Log/2026-07-30.md` — "Boot ABI profile 化与静态设备目录移除"

## Execution Context

```text
git describe --always --dirty: aa46a09b-dirty
container: 79040401ef8a
mount: /root/projects/MangoCore -> /app
os_test.conf sha256: f92b83ed13bd28eec1c0903a8a3441f9c6c4cc252f4eda13226ca7b15a6a9218
```

All build and QEMU commands below ran in `/app` inside the named Docker
container. The host workspace is the sole project mount.

## Commands and Exit Status

```bash
bash scripts/test-canonical-entrypoint-contract.sh
make -C os -n la64-2k1000-core-tests | grep -- "--features" | grep -qv board_
make -C os -n la64-2k1000-shell | grep -- "--features" | grep -qv board_
make toolchain-preflight
make kernel ARCH=rv64 PROFILE=normal
make kernel ARCH=la64 PROFILE=normal
make -C os ktest-run ARCH=rv64 PROFILE=normal \
  KTEST=platform,platform_resources,platform_fdt_snapshot KTEST_QEMU_TIMEOUT=60
make test ARCH=rv64 PROFILE=regression
```

All commands exited with status `0`. RV64 and LA64 kernel builds were run in
that order, never concurrently.

The required raw artifacts are retained beside this record:
`git-hash.txt`, `container-id.txt`, `config.txt`, `commands.txt`,
`qemu-output.log`, and `qemu-head-tail.txt`.

## Canonical Entry-Point Contract

The contract checks the normal and regression `check`, `ktest-build-only`, and
`ktest-run` dry-runs for both architectures. It confirmed exactly one profile
per kernel Cargo invocation and rejected an invalid or omitted `ARCH`/`PROFILE`.
The 2K1000 uImage dry-runs also produced no `board_` entry in their Cargo
feature list.

## RV64 QEMU ktest Result

```text
[kernel] Boot protocol: RiscvFdt, hart_id=0, dtb_paddr=0x82200000
[kernel] discovered VirtIO block device (MMIO 0x10001000)
[kernel] block device vda: Root
[kernel] root device mounted at sdcard
TAP version 13
1..13
ok 1-7 platform::*
ok 8-11 platform_fdt_snapshot::*
ok 12-13 platform_resources::*
# results: 13 passed, 0 failed, 13 total
[KTEST RESULT: PASS]
# ktest: shutting down.
```

The ktest QEMU command does not attach a NIC, so the observed loopback-only
network initialization is expected for this harness invocation.

## RV64 Regression QEMU Result

```text
[kernel] Boot protocol: RiscvFdt, hart_id=0, dtb_paddr=0x82200000
[kernel] Regression mode — skipping block init
TAP version 13
1..23
ok 1-22
ok 23 pipe_resize # SKIP known kernel bug: resize wakeup
# results: 22 passed, 0 failed, 1 skipped, 23 total
[L4 REGRESSION RESULT: PASS]
=== REGRESSION PASS ===
```

`pipe_resize` is the pre-existing documented skip. No new failures occurred.

## Scope Limit

`boot_rv_uboot_go` intentionally does not consume `a1` as a DTB. VF2 needs a
separate `booti <Image> - <DTB>` handoff implementation before FDT runtime
discovery can be enabled there. LA64 static boot behavior remains an ABI-bound
path and does not synthesize `PlatformInfo.devices`.
