# SMP-P0-B01 evidence manifest

## Result

- `SMP-P0-B00`: **partial** — branch/dirty state, container provenance,
  QEMU versions, and pre-change dual-architecture builds were captured.
  Registry pull and container force-recreation were not performed in this
  batch, so image freshness relative to the remote tag is not claimed.
- `SMP-P0-B01`: **pass** — the build-time CPU-count contract is implemented
  and the required build/runtime checks passed.
- Baseline: branch `smp`, commit `d764f885`, dirty before this batch because
  of pre-existing user files including `docs/Work_Log/2026-07-19.md`.

## Scope

- `os/build.rs`
- `os/make/rv64.mk`
- `os/make/la64.mk`

No QEMU command line, boot entry, scheduler, lock, interrupt, page table, or
runtime SMP code was changed.

## Environment

- Verification container: `lzm-cagent-run`
- Mount: `/home/lzm/projects/MangoCore -> /app`
- Image ID: `sha256:a89ceaf40ef5049b5103f7f0685311c3b499d56781bdc4c9605bf4ac597dd581`
- Recorded digest:
  `zhouzhouyi/os-contest@sha256:5c04dbc38562b1cd578c33c9cd321d4731cb8cdd00c82b2320a4350754faa6b0`
- RV64 QEMU: 10.0.2
- LA64 QEMU: 10.0.2

The running `pxy-mangocore-os-dev-1` container was rejected for verification
because it mounts `/home/pxy/projects/MangoCore`, whose commit was different
from the current worktree.

## Validation summary

| Check | Result |
|---|---|
| Pre-change RV64 kernel build | PASS, exit 0 |
| Pre-change LA64 kernel build | PASS, exit 0 |
| Both Makefiles accept 1/2/4/8 | PASS |
| Both Makefiles reject 3 | PASS, expected exit 2 |
| Top-level RV64/LA64 targets reject 3 | PASS, expected exit 2 |
| Both Makefiles export `MANGO_CORE_NUM=2` | PASS |
| Direct `build.rs`, value 2 | PASS, rustc env emitted |
| Direct `build.rs`, value 3 | PASS, expected exit 101 with actual value |
| Post-change RV64 `CORE_NUM=1` build | PASS, exit 0 |
| Post-change LA64 `CORE_NUM=1` build | PASS, exit 0 |
| RV64 waitqueue ktest | PASS, 4/4 |
| LA64 waitqueue ktest | PASS, 4/4 |
| Diff whitespace check | PASS |
| Evidence freshness | PASS |

The RV64 single-core boot log reports `Platform HSM Device: ---`. This does
not affect this build-contract batch, but it is a mandatory firmware gate to
resolve before the later RISC-V AP-start batch.

## Evidence index

- `smp-P0-B01-container.txt`: container, mount, image, QEMU, and Rust metadata
- `smp-P0-B01-config.txt`: test configuration
- `smp-P0-B01-commands.txt`: commands executed
- `smp-P0-B01-make-contract.log`: accepted/rejected Make values
- `smp-P0-B01-top-level-reject.log`: real entry-point rejection checks
- `smp-P0-B01-export-contract.log`: exported environment checks
- `smp-P0-B01-build-script-contract.log`: direct build-script checks
- `smp-P0-B01-*-build-after.log`: complete post-change build logs
- `smp-P0-B01-*-qemu-output.log`: complete build and QEMU output
- `smp-P0-B01-*-qemu-head-tail.txt`: compact log witnesses
- `smp-P0-B01-freshness.txt`: source/evidence timestamp comparisons
- `smp-P0-B01-diff-numstat.txt`, `smp-P0-B01-diff-u0.patch`: raw line ledger
- `smp-P0-B01-line-ledger.md`: semantic line classification
