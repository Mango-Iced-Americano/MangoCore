# SMP-P1-B05 evidence manifest

- Batch: `SMP-P1-B05`
- Status: `pass`
- Baseline HEAD: `4cb1f08fb1d1920b3269fbccd5c9482c953869ec`
- Container: `lzm-cagent-run`
- Image: `zhouzhouyi/os-contest:20260104`
- Image ID: `sha256:a89ceaf40ef5049b5103f7f0685311c3b499d56781bdc4c9605bf4ac597dd581`
- Container created: `2026-07-19T11:28:07.697595896Z`
- RV64 QEMU: `10.0.2`
- LA64 QEMU: `10.0.2`

## Invariant

After every user-to-kernel trap, Rust observes the current CPU's validated
PerCpu pointer in RV64 `tp` or LA64 `$r21`; the user's saved register value is
restored unchanged on return.

## Validation

1. Docker build-only commands were executed serially with `CORE_NUM=2`.
   Both compiler/linker processes exited 0 and produced their kernel artifacts.
2. The deterministic wrapper marked its final RV64 and LA64 build jobs `FAIL`
   solely because the Makefiles copy architecture-specific tracked
   `lang_items.rs` and LA64 `linker.ld` templates during a build. The associated
   `result.json` files record `process_exit_code: 0`, no forbidden marker, and
   `mutation_detected: true`. This wrapper-integrity result is preserved rather
   than rewritten as PASS.
3. The repository's user-mode regression image was built with
   `MANGO_CORE_NUM=2`. Its stock QEMU recipe hard-codes `-smp threads=1`, so the
   same artifacts were launched manually inside Docker with
   `-smp cpus=2,sockets=1,cores=2,threads=1`.
4. RV64 and LA64 both reported `configured=2`, `online_mask=0x3`, completed all
   six user regression tests, printed `L4 REGRESSION RESULT: PASS`, and shut
   down with status 0. Neither log contains a panic or CPU-local validation
   failure.
5. Final ELF disassembly proves the required ordering:
   - RV64 saves user `tp` at `0x20(sp)`, later loads kernel `tp` from
     `0x230(sp)` (slot 70), then loads kernel `sp` from slot 69.
   - LA64 saves user `$r21` at `168(sp)`, stores LSX from aligned offset 576,
     later loads kernel `$r21` from `560(sp)` (slot 70), then loads kernel
     `$sp` from slot 69.

`CORE_NUM=1/4/8` user regression was not run: the CPU-local ownership transition
is exercised on CPU0 in the two-core run, while the additional AP remains
parked. More parked CPUs would not add a new trap concurrency path in Phase 1.

## Artifacts

- `b05-{rv64,la64}-build-result.json` — deterministic wrapper result.
- `b05-{rv64,la64}-build.{stdout,stderr}.log` — complete build output.
- `b05-{rv64,la64}-regression-build.log` — complete regression-kernel build.
- `b05-{rv64,la64}-regression-qemu.log` — complete two-core QEMU console.
- `b05-{rv64,la64}-alltraps.disasm` — final `__alltraps` disassembly.

The LA64 build-generated `linker.ld` change was restored with an exact hunk.
Pre-existing tracked changes in `os/src/lang_items.rs` and
`user/src/lang_items.rs` were not manually edited or included in B05.
