# SMP-P1-B03 evidence manifest

## Batch

- Name: `SMP-P1-B03`
- Goal: dual-architecture minimal BSP/AP boot closure for 1/2/4/8 CPUs
- Stage result: `pass`
- Overall Phase 1: `partial`
- User-approved exception: this batch is not limited to about 50 critical lines
- Scheduler/TLB/FS/net/driver SMP: out of scope
- Actual test container: `lzm-cagent-run`,
  `zhouzhouyi/os-contest:20260104`,
  image ID `sha256:a89ceaf40ef5049b5103f7f0685311c3b499d56781bdc4c9605bf4ac597dd581`
- Actual test QEMU: RV64 10.0.2, LA64 10.0.2

## Official references checked before the final fix

- QEMU v9.2.1 LoongArch direct-boot ROM:
  <https://github.com/qemu/qemu/blob/v9.2.1/hw/loongarch/boot.c>
- QEMU v9.2.1 common IPI implementation:
  <https://github.com/qemu/qemu/blob/v9.2.1/hw/intc/loongson_ipi_common.c>
- QEMU v9.2.1 common IPI register constants:
  <https://github.com/qemu/qemu/blob/v9.2.1/include/hw/intc/loongson_ipi_common.h>
- QEMU 9.2 invocation documentation (`-accel`, TCG `thread`, `-smp`):
  <https://qemu.readthedocs.io/en/v9.2.0/system/invocation.html>
- OpenSBI official README (HSM and booting-hart contract):
  <https://github.com/riscv-software-src/opensbi>

## RED evidence retained

- `smp-P1-B03-rv64-2core-red-qemu.log`: custom OpenSBI sees two harts but
  no AP online marker before HSM implementation.
- `smp-P1-B03-rv64-2core-red-default-sbi-qemu.log`: the same missing AP
  closure with QEMU default OpenSBI.
- `smp-P1-B03-la64-2core-green-prelim.log`: intentionally retained
  misleading exit status 0 with kernel online-timeout panic; proves QEMU process
  status alone is not a valid judge.
- `smp-P1-B03-rv64-4core-final.log`: OpenSBI selects Boot HART 2 and the
  pre-mapping kernel times out before output.
- `smp-P1-B03-rv64-8core-competition-command.log`: used a stale
  `../kernel-rv`; retained to document artifact freshness failure.
- `smp-P1-B03-elf-layout-check.status`: first assertion script failed because
  `[` did not accept the hexadecimal RHS; superseded by the fail-closed final
  arithmetic check.

## Final build gates

| Evidence | Result |
|---|---|
| `smp-P1-B03-rv64-build-final.log/.status` | `0` |
| `smp-P1-B03-la64-build-final.log/.status` | `0` |
| `smp-P1-B03-2k1000-singlecore-build.log/.status` | `0` |

RV64 and LA64 QEMU-target builds ran sequentially in `lzm-cagent-run`.
The additional 2K1000LA single-core release build verifies that the common
two-argument `rust_main` ABI still compiles for the explicitly out-of-scope
board; it is not a board-runtime SMP claim.
After correcting the module-header comment from “two” to “three” startup
atomics, the RV64 and LA64 `CORE_NUM=1` build gates were run sequentially once
more and both commands returned zero.

## Final focused QEMU matrix

Each judge requires all of:

1. exact configured CPU count and expected online mask;
2. `KTEST RESULT: PASS`;
3. no `panicked at`;
4. build/QEMU command success.

| Arch | CPUs | Final log/status | Online mask | Result |
|---|---:|---|---:|---|
| RV64 | 1 | `rv64-1core-postmap` | `0x1` | pass |
| RV64 | 2 | `rv64-2core-postmap` | `0x3` | pass |
| RV64 | 4 | `rv64-4core-final2` | `0xf` | pass |
| RV64 | 8 | `rv64-8core-final` | `0xff` | pass |
| LA64 | 1 | `la64-1core-final` | `0x1` | pass |
| LA64 | 2 | `la64-2core-final` | `0x3` | pass |
| LA64 | 4 | `la64-4core-final` | `0xf` | pass |
| LA64 | 8 | `la64-8core-final` | `0xff` | pass |

All rows run `KTEST=waitqueue KREPEAT=1`; the existing four tests pass.
This is a boot-preservation focused test, not the future dedicated `KTEST=smp`.

## Competition-command semantics

- `smp-P1-B03-rv64-8core-competition-command2.log/.status`:
  no explicit `-accel`, `-bios default`, `-smp 8`; QEMU default OpenSBI 1.5.1
  selects Boot HART 6, online mask reaches `0xff`, KTEST passes.
- `smp-P1-B03-la64-8core-competition-command.log/.status`:
  no explicit `-accel`, `-smp 8`; boot hardware ID 0, online mask reaches
  `0xff`, KTEST passes.

These runs establish that omitting `-accel` does not justify a physical-hart0
assumption.

The command shape matches the supplied competition invocation, but the local
runtime is QEMU 10.0.2. QEMU v9.2.1 official source was used to validate the
LoongArch register protocol; no accessible 9.2.1 container remained for a
runtime rerun, so this manifest does not claim a 9.2.1 execution result.

## ELF/layout gates

- `smp-P1-B03-elf-layout.txt`: raw symbols for both final target ELFs.
- `smp-P1-B03-elf-layout-check-final.txt/.status`: both boot-stack arrays span
  `0x200000` and end exactly at `sbss`.
- `smp-P1-B03-data-boot-check.txt/.status`:
  `BOOT_HARDWARE_ID`, `BOOT_PHASE`, and `ONLINE_MASK` are inside
  `[sdata, edata)` for both architectures.

## Diff and documentation

- `smp-P1-B03-code-diff.patch`
- `smp-P1-B03-code-numstat.txt`
- `smp-P1-B03-line-ledger.md`
- `docs/01_architecture/boot-and-trap.md`
- `docs/10_plan/smp-8core-implementation.md`
- `docs/Work_Log/2026-07-24.md`
- `.agents/skills/mango-workflow/references/debugging-patterns.md`

## Explicit non-claims

- APs do not run tasks and do not enter the legacy global scheduler.
- PerCpu, idle context, normal IPI handlers, shootdown, affinity, migration and
  shared-subsystem SMP safety are not implemented by this batch.
- LoongArch AP park is a temporary spin hint; RISC-V AP park is permanent WFI.
- 2K1000LA remains single-core.
