# lang-items source-purity hardening evidence — 2026-07-20

- Container: `c238e449081e4c68d07b193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`
- Toolchain environment: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`
- Container `.git` worktree metadata points at a host-only path, so this record intentionally does not assert a Git revision.

## Regression guard

`sh scripts/test-source-purity-make-contract.sh` passed against the repository. The contract discovers and sorts first-party `Makefile`, `GNUmakefile`, and `*.mk` files while excluding `.git`, `dependency`, and the untracked external cache at `user/tools`.

Its isolated child fixture places a forbidden recipe in `modules/variant-copy.mk`, which is outside `os/Makefile` and therefore demonstrates the gap in the former OS-only scan. The fixture confirms rejection with `file:line` diagnostics for:

1. plain `cp`
2. `-@cp`
3. `@ cp`
4. `$(CP)`
5. `/bin/cp`
6. quoted variant and destination paths

Both active workflows invoke `sh scripts/test-source-purity-make-contract.sh` from their existing `toolchain-contracts` shell block under `set -euo pipefail`.

## Docker validation

```sh
docker compose exec -T \
  -e RUSTUP_HOME=/root/.rustup \
  -e CARGO_HOME=/root/.cargo \
  os-dev sh -lc '
    cd /app
    sha256sum os/src/lang_items.rs user/src/lang_items.rs \
      os/src/lang_items.rs.rv os/src/lang_items.rs.la \
      user/src/lang_items.rs.rv user/src/lang_items.rs.la
    sh scripts/test-source-purity-make-contract.sh
    make -C os toolchain-preflight
    make -C os rv64-kernel-build-only
    make -C os la64-kernel-build-only
    sha256sum os/src/lang_items.rs user/src/lang_items.rs \
      os/src/lang_items.rs.rv os/src/lang_items.rs.la \
      user/src/lang_items.rs.rv user/src/lang_items.rs.la
  '
```

Result: source-purity and preflight passed; RV64 completed before LA64 began; both release kernel builds completed. Pre/post SHA-256 values were identical:

- `os/src/lang_items.rs`: `5ec4733d951be7fc5e48256fd9932bde84cde665f7f922b0c88c46b358efb2ec`
- `user/src/lang_items.rs`: `b4dd3f83d45d7dcfe2859c42a11b2389e73dd1a0012ebd06bb8ed84915edbdde`
- `os/src/lang_items.rs.rv`: `3bd52e485146d86e10ef2c62446c14c94425a0e7002bb3132dc2c58135ccd612`
- `os/src/lang_items.rs.la`: `5ec4733d951be7fc5e48256fd9932bde84cde665f7f922b0c88c46b358efb2ec`
- `user/src/lang_items.rs.rv`: `b16077066c18eed2b5d5c0ffa817a3abf0ec393323a59c5c4ac4f9c79c510345`
- `user/src/lang_items.rs.la`: `b4dd3f83d45d7dcfe2859c42a11b2389e73dd1a0012ebd06bb8ed84915edbdde`

No QEMU command was run or claimed.
