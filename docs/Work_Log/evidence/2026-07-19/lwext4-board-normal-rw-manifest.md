# 2K1000LA lwext4 P4 normal-RW and cold-reboot gate

## Identity and safety boundary

- Tested source commit: `9b02dda580ef7135b52d0c155fca664b0bab382f`
- Branch: `board-develop-combined`
- Docker image: `zhouzhouyi/os-contest:20260104`
- Board image: `kernel-2k1000-persist-shell.ui`
- Board image SHA-256: `6eba44cd74714e3e43bdac9605cbe94ddc678dabf177fbcd50fdf8d65bd89f41`
- uImage payload: `16,930,776` bytes; load/entry `0x90000000`; data CRC `1a807ea1`
- Full-SSD backup verified before this run: source size `32,017,047,552` bytes, original-stream SHA-256 `815df871d006032eec47c1fd1b44dded43ba4c2618a07bf8a1b49ae1de930b08`, compressed-image SHA-256 `ea14dfabb08a9047d671eac0a300c8be8b0f5c7ad75c84b5bff1d38904ff3f95`.
- The image kept P1 `/sdcard` and P3 `/tools` read-only. `/dev/sda` and `/dev/sda1` through `/dev/sda4` were all `br--r-----`; only the fixed identity-checked P4 was mounted read-write at `/persist`.
- No U-Boot `saveenv` or `scsi write` command was used. All P4 writes were filesystem-level operations in uniquely named temporary files, followed by explicit cleanup and `sync`.

## Boot evidence

Both boots transferred exactly `16,930,840` uImage bytes by TFTP, matched CRC32 `08e52445`, passed `iminfo` LoongArch/checksum validation, and booted with `bootm 0x9000000098000000`.

- First boot log: `board-lwext4-p4-normal-run.log`, SHA-256 `6c12bffb05f5eb92e6c82fc1d72e286905d9a776f791cb0c99850459046f465a`
- Cold-reboot log: `board-lwext4-p4-normal-reboot.log`, SHA-256 `055c046560f83153e8620cdd8a3000532cfa662b61a268c193313113f97b4337`

The raw logs cover U-Boot/TFTP verification. The shell assertions below were captured interactively after each boot; commands were paced byte-by-byte because this board's USB-UART input loses characters when a full line is injected at once.

## Filesystem gate

Temporary root: `/persist/.mango_lwext4_gate_9b02dda5` (confirmed absent before use).

1. `mkdir`, deterministic write, `sync`, rename `before -> after`, second `sync`.
2. `sha256sum after` returned `3994cd6202016147f793c973d14dded4a67328acbc799955bb8b7d739ced373f`.
3. Opened `open` on fd 3, wrote `first`, synced, unlinked the pathname while fd 3 remained live, asserted `UNLINKED`, wrote `second` through fd 3, closed it, synced, asserted `FINALIZED`.
4. Cold-reset and TFTP-booted the exact same kernel again. `after` retained the exact SHA-256 above and the open-unlink pathname remained absent (`NO_ORPHAN_PATH`).
5. Removed `after`, removed the temporary directory, synced, and asserted `CLEAN`.

This P4 intentionally has no journal under the established board identity policy. Therefore this run validates lwext4/SATA normal write, sync, rename, runtime open-unlink, final-close reclamation, cold-remount persistence, and cleanup on real hardware; deterministic journal replay/persistent-orphan recovery remains covered by the disposable QEMU two-boot matrix.

## Bounded performance sample

- Workload: BusyBox `dd`, one 16 MiB temporary file under `/persist`, `bs=1M`.
- Write with `conv=fsync`: `16,777,216` bytes in `1.803064 s`, `8.9 MB/s`.
- Read to `/dev/null`: `16,777,216` bytes in `0.143612 s`, `111.4 MB/s`.
- The file was removed, `sync` completed, and `PERF_CLEAN` was asserted.

This is a smoke sample, not a controlled old-vs-new benchmark. The persistent-orphan change does not enter normal read/write on a no-journal volume; journaled zero-link metadata overhead must be measured separately on a journal-enabled disposable target.
