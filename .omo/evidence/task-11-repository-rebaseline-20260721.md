# T11 repository rebaseline evidence

## Scope

Documentation, CI, Docker metadata, and script comments were aligned with the T1–T10 contracts. No kernel or userspace behavior changed.

## Updated contracts

- Root Make facade commands use explicit `ARCH` and `PROFILE`.
- Boot documentation records CPIO → `VFS_ROOT` → `/init` → `/sbin/init` → test runner.
- PID1 owns normal mounts, including ext4-backed `/tmp` before tmpfs fallback.
- Image roles record x0=rootfs/sdcard and x1=tools ext4 P1 plus FAT32 scratch P2.
- CI uses the compose container tag, serial full-test runner, `make all`, and `make lint`.

## Verification

- `scripts/test-rebaseline-doc-contract.sh`: not present in this checkout.
- Docker Compose service `pxy-cleanup-os-dev-1`, repository mount `/home/pxy/projects/MangoCore-cleanup -> /app`.
- Docker `make toolchain-preflight` reported `nightly-2026-05-10`.
- Docker dry-ran all documented formal targets: dual-arch `kernel`, `build`, `check`, OS-level `ktest-run`, regression `test`, and four-cell `lint`.
- `git diff --check` passed.

## Scan note

The requested stale-token scan still reports historical work material, archived plans, and intentional fixture assertions outside the maintained documentation scope. Current AGENTS, root README, architecture/FS docs, CI, compose, and initramfs script comments no longer present these contracts as active behavior.
