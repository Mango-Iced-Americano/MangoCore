# lang-items source-purity cleanup evidence — 2026-07-20

- Container: `c238e449081e4c68d07b193d7e7e7c357406503eaab22305cc763a4d9c2e1161`
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`
- Toolchain environment: `RUSTUP_HOME=/root/.rustup`, `CARGO_HOME=/root/.cargo`
- Repository revision metadata was unavailable inside the container because its `.git` worktree pointer references a host-only path; no revision is asserted here.

## RED contract

Before deleting the legacy recipes, `./scripts/test-source-purity-make-contract.sh` exited `1` and reported 33 commands that copied a `.rv` or `.la` lang-items variant into a tracked active file.

## GREEN command

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

## Result

- The source-purity contract passed.
- RV64 completed before LA64 started; both `release` kernel builds completed successfully.
- Pre/post SHA-256 values were identical:
  - `os/src/lang_items.rs`: `5ec4733d951be7fc5e48256fd9932bde84cde665f7f922b0c88c46b358efb2ec`
  - `user/src/lang_items.rs`: `b4dd3f83d45d7dcfe2859c42a11b2389e73dd1a0012ebd06bb8ed84915edbdde`
  - `os/src/lang_items.rs.rv`: `3bd52e485146d86e10ef2c62446c14c94425a0e7002bb3132dc2c58135ccd612`
  - `os/src/lang_items.rs.la`: `5ec4733d951be7fc5e48256fd9932bde84cde665f7f922b0c88c46b358efb2ec`
  - `user/src/lang_items.rs.rv`: `b16077066c18eed2b5d5c0ffa817a3abf0ec393323a59c5c4ac4f9c79c510345`
  - `user/src/lang_items.rs.la`: `b4dd3f83d45d7dcfe2859c42a11b2389e73dd1a0012ebd06bb8ed84915edbdde`
- QEMU was not run: this change removes Makefile writes and does not alter runtime behavior.
