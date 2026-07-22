# T10 — repository warning rebaseline

## Scope

Re-established the kernel warning baseline after T1–T9 removals and made the
first-party warning gate fail closed.

## Environment

- Docker service: `os-dev` (`a317-tower1`)
- Mount: `/home/pxy/projects/MangoCore-cleanup -> /app`
- Toolchain homes: `/root/.rustup`, `/root/.cargo`
- Source revision at measurement: `2601ab32` plus uncommitted T10 changes

## Four-cell baseline

Fresh Docker kernel builds were collected serially with
`make -f make/{rv64,la64}.mk kernel MODE={debug,release}`. The normalized
machine-readable baselines were then produced by
`scripts/lint-check.sh --capture-baseline`.

| Cell | First-party (`os/src/`) | Maintained (`libs/mango-kernel-core/`) | Vendor (`dependency/`) |
|---|---:|---:|---:|
| RV64 debug | 173 | 0 | 35 |
| RV64 release | 173 | 0 | 35 |
| LA64 debug | 177 | 0 | 36 |
| LA64 release | 177 | 0 | 36 |

`lint-baseline/*.txt` records `<ownership>:<lint>:<path>` tuples. Vendor
warnings remain informational and are not modified or gated as first-party.

## Gate behavior

- Root `make lint` runs all four cells when `ARCH` is omitted; an explicit
  `ARCH`/`MODE` runs exactly that cell.
- `make check` propagates `MODE=release` to `cargo check --release` for both
  architecture Makefiles.
- `os/src/main.rs` denies `future_incompatible`, `improper_ctypes`,
  `clashing_extern_declarations`, `unexpected_cfgs`, and `static_mut_refs`
  for the first-party kernel crate without escalating vendor warnings.
- The negative fixture injected `os/src/lint_fixture.rs`; `make lint
  ARCH=rv64 MODE=debug` returned nonzero and printed
  `new first-party warning: unknown in src/lint_fixture.rs`.

## Verification

All commands ran in the Docker service, serially:

```text
make lint ARCH=rv64 MODE=debug
make lint ARCH=rv64 MODE=release
make lint ARCH=la64 MODE=debug
make lint ARCH=la64 MODE=release
sh scripts/test-lint-gate-fixture.sh --inject unused-first-party
make check ARCH=rv64 PROFILE=normal MODE=release
make check ARCH=la64 PROFILE=normal MODE=release
make -C os rv64-kernel-build-only PROFILE=normal MODE=release
make -C os la64-kernel-build-only PROFILE=normal MODE=release
```

All positive commands exited 0. The fixture intentionally made its inner
lint invocation fail and itself exited 0 only after confirming that failure.
No QEMU run was required for this build/lint-only gate change.
