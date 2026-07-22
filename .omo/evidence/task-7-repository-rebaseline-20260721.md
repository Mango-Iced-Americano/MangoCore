# T7 — userspace init lifecycle split

**Date:** 2026-07-22

## Delivered contract

- `/sbin/init` is `initd`: PID1-only lifecycle supervision, minimal pseudo-fs framework, boot-profile read, SIGCHLD reaping, shutdown handling, and rescue-shell fallback.
- `/usr/libexec/mangocore/test-runner` is the non-PID1 competition runner.  It rejects PID1 or a non-PID1 parent before bootstrapping compatibility mounts and executing existing test policy.
- `/init` and `/initproc` are thin exec shims to `/sbin/init`.
- `scripts/test-init-lifecycle-contract.sh` guards the source split and package paths.

## Docker environment

- Container: `c238e449081e4c68d07d193d7e7c357406503eaab22305cc763a4d9c2e1161` (`pxy-cleanup-os-dev-1`)
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`
- Toolchain homes: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`

## Commands and results

| Command | Result |
|---|---|
| `sh scripts/test-init-lifecycle-contract.sh && sh -n os/build_initramfs.sh` | pass |
| `make -C os user ARCH=rv64 PROFILE=normal` | pass |
| `make -C os user ARCH=la64 PROFILE=normal` | pass |
| `make -C os rv64-kernel-build-only` | pass |
| `make -C os la64-kernel-build-only` | pass |
| `cpio -it < /app/build/rv64/release/normal/initramfs/initramfs-rv.cpio` | pass; contains `init`, `initproc`, `sbin/init`, and `usr/libexec/mangocore/test-runner` |
| `make -C os boot-smoke ARCH=rv64 PROFILE=normal` | unavailable: target is absent |
| `timeout 180s make -C os rv64-run` | QEMU invoked but could not start because required `../sdcard-rv.img` is absent; no userspace boot result claimed |

Existing repository warnings remain in the user and kernel builds; no T7 compile error occurred.
