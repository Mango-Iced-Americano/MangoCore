# SMP-P1-B01 evidence manifest

## Result

- Batch `SMP-P1-B01`: **pass**.
- Overall Phase 1: **partial**. This batch establishes only the RV64
  pre-Rust per-hart boot-stack invariant.
- Baseline: branch `smp`, commit
  `d764f8853487848c8ab5559880447cd7425d3c80`, dirty before the batch due to
  preserved user changes and the preceding `SMP-P0-B01` batch.

## Scope and design

`os/src/hal/arch/riscv/entry.asm` now reserves eight 256 KiB slots and derives
the initial stack pointer from the OpenSBI hart ID in `a0`. The unsigned range
check occurs before the first stack-dependent instruction. IDs outside
`0..7`, plus an unexpected return from divergent `rust_main()`, enter the same
stack-free `wfi` park loop.

The historical `boot_stack_top` symbol remains the upper bound of CPU0's slot
so panic backtrace bounds do not silently expand to two MiB. The new
`boot_stack_upper_bound` marks the full array, and the linked ELF proves that
it equals `sbss`; normal BSS clearing therefore starts after all eight slots.

This batch does not start APs, split `rust_main()`, change LoongArch startup,
enable a multi-core QEMU topology, or touch traps, scheduling, locks, IPI, MM,
TLB, filesystems, networking, or drivers.

## Validation summary

| Check | Result |
|---|---|
| Pre-change ELF witness | RED: one 0x40000 stack and no hart-based selection |
| RV64 `CORE_NUM=1` kernel build | PASS, exit 0 |
| LA64 `CORE_NUM=1` kernel build | PASS, exit 0 |
| CPU0 slot size | PASS, `0x40000` |
| Full stack array size | PASS, `0x200000` |
| Array upper bound equals `sbss` | PASS |
| Entry instruction pattern/order | PASS |
| RV64 waitqueue ktest | PASS, 4/4 |
| Generated linker/lang-item selections restored | PASS |
| Diff whitespace check | PASS |
| Evidence freshness | PASS |
| RV64/LA64 2/4/8-core runtime | NOT RUN by design |

The single-core QEMU log still reports `Platform HSM Device: ---`. It is not a
failure for stack reservation, but it remains an explicit blocker to resolve
before an RISC-V HSM AP-start batch can be accepted.

## Environment

- Verification container: `lzm-cagent-run`
- Mount: `/home/lzm/projects/MangoCore -> /app`
- Image ID:
  `sha256:a89ceaf40ef5049b5103f7f0685311c3b499d56781bdc4c9605bf4ac597dd581`
- Recorded digest:
  `zhouzhouyi/os-contest@sha256:5c04dbc38562b1cd578c33c9cd321d4731cb8cdd00c82b2320a4350754faa6b0`
- RV64 and LA64 QEMU: 10.0.2

No registry pull or force-recreation was performed in this batch, so remote
tag freshness is not claimed.

## Evidence index

- `smp-P1-B01-red-before.txt`: pre-change ELF symbols and `_start`
- `smp-P1-B01-*-build.log`: complete serial dual-architecture build logs
- `smp-P1-B01-layout-after.txt`: post-change symbols and disassembly
- `smp-P1-B01-layout-check.txt`: arithmetic and instruction assertions
- `smp-P1-B01-slot-map.txt`: all eight contiguous, disjoint slot intervals
- `smp-P1-B01-rv64-qemu-output.log`: complete focused QEMU run
- `smp-P1-B01-rv64-qemu-head-tail.txt`: compact QEMU witness
- `smp-P1-B01-rv64-qemu-key-markers.txt`: topology/HSM/result markers
- `smp-P1-B01-diff-numstat.txt`, `smp-P1-B01-diff-u0.patch`: raw code diff
- `smp-P1-B01-line-ledger.md`: semantic critical-line classification
- `smp-P1-B01-doc-sync.patch`: architecture/plan synchronization
- `smp-P1-B01-container.txt`: container, image, QEMU, and toolchain metadata
- `smp-P1-B01-freshness.txt`: source/evidence timestamp proof
- `smp-P1-B01-git-*.txt`: baseline and post-batch worktree state
- `smp-P1-B01-final-audit.txt`: aggregate exit-code and restoration audit
