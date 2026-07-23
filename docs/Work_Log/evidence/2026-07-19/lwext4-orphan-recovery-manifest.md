# lwext4 persistent orphan / power-loss recovery evidence

## Scope and identity

- Branch: `board-develop-combined`
- Base commit: `a6ab5ba0c4fe1a4973ff586303ae1d669ebc73cd`
- Docker image: `zhouzhouyi/os-contest:20260104`
- Docker image ID: `sha256:5c04dbc38562b1cd578c33c9cd321d4731cb8cdd00c82b2320a4350754faa6b0`
- All writable tests used disposable 64 MiB files under this evidence directory and QEMU VirtIO block devices. The production-board P4 partition was neither mounted nor written.
- Fixtures explicitly enabled `has_journal` and disabled the unsupported `orphan_file` incompat feature. The 4 KiB and 1 KiB block-size paths were both exercised.

## Deterministic two-boot procedure

1. Create a fresh ext4 fixture with `mke2fs -t ext4 -F -b <4096|1024> -m 0 -O has_journal,^orphan_file`.
2. Boot `mango.test=ext4_orphan_crash`. The test writes and syncs a live file, arms the lwext4 journal fault hook, then unlinks it.
3. The hook parks after the transaction records, commit block, and journal start pointer are durable, but before home-block checkpoint. The host timeout terminates QEMU at the marker:
   `[KTEST ORPHAN CRASH] unlink transaction entering power-cut window`.
4. Reuse the exact same image (`KTEST_EXT4_REUSE=1`) and boot `mango.test=ext4_orphan_recover`.
5. Mount recovery must replay the unlink, recover exactly one orphan, keep the old pathname absent, pass a write/read/delete probe, complete filesystem teardown, and print `KTEST RESULT: PASS`.
6. After QEMU exits, run read-only `e2fsck -f -n`; all five passes must complete without repairs or errors.

The Makefile controls used by this procedure are `KTEST_EXT4_IMG_{RV,LA}`, `KTEST_EXT4_FEATURES`, `KTEST_EXT4_BLOCK_SIZE`, `KTEST_EXT4_REUSE`, and `KTEST_POST_FSCK`.

## RED baseline

- Image: `rv64-lwext4-orphan-red.img`
- SHA-256: `2d722c4fb4aed04331a659b8b2fa2831f06d9ac4ebb6efe8e5a4734954e8e51a`
- Offline fsck: `rv64-lwext4-orphan-red-fsck.log`
- Result: `Deleted inode 12 has zero dtime`, block bitmap leak `-2065`, inode bitmap leak `-12`, filesystem still has errors, `e2fsck_exit=4`.

This proves that the previous in-memory open-unlink lifecycle did not provide crash-persistent orphan recovery.

## GREEN matrix

| Architecture / format | Crash log | Same-image recovery log | Final image SHA-256 | Result |
|---|---|---|---|---|
| RV64, ext4 4 KiB | `rv64-lwext4-orphan-crash.log` | `rv64-lwext4-orphan-recover.log` | `9e37020a3b2918df1255b6d1bdd3f48b252412bcdcb3573138b9e224f68471b4` | one orphan recovered; writable probe, teardown, and fsck pass |
| LA64, ext4 4 KiB | `la64-lwext4-orphan-crash.log` | `la64-lwext4-orphan-recover.log` | `e59f46366a04e224ce92716bb1f11f8aee383d792503509953ada572961c5c62` | one orphan recovered; writable probe, teardown, and fsck pass |
| RV64, ext4 1 KiB | `rv64-lwext4-orphan-1k-crash.log` | `rv64-lwext4-orphan-1k-recover.log` | `ac15b6a9ae9af7ce5a5b87ef28ed61c1d8e27856ba2ed31d0f8ca5406a053fff` | superblock-at-logical-block-1 replay path, teardown, and fsck pass |

Normal ext4 regression also passed 8/8 with clean teardown and offline fsck on both architectures (`rv64-lwext4-ext4-regression.log`, `la64-lwext4-ext4-regression.log`). The final exact source compiled successfully for LA64 in `la64-lwext4-orphan-final-build.log`; RV64 exact-source compilation is included in the final crash/recovery runs.

The large raw images and verbose build/QEMU logs are retained locally beside this manifest but intentionally excluded from Git. The hashes above identify the tested images without permanently adding hundreds of MiB to repository history.
