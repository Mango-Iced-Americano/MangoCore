# SMP-P1-B02 evidence manifest

## Result

- Batch `SMP-P1-B02`: **pass**.
- Overall Phase 1: **partial**. This batch establishes only the LA64 QEMU
  pre-Rust per-CPU boot-stack invariant.
- Baseline: branch `smp`, commit
  `d764f8853487848c8ab5559880447cd7425d3c80`, dirty before the batch due to
  preserved user changes and preceding approved SMP batches.

## Scope and design

`os/src/hal/arch/loongarch64/boot.rs` now reserves eight 256 KiB slots. After
programming DMW and before assigning `$sp`, `_start` reads the CPU-local
`CSR.CPUID`, rejects IDs outside `0..7`, and selects
`BOOT_STACK + (cpu_id + 1) * BOOT_STACK_SIZE` as the downward-growing stack's
exclusive upper bound. Invalid IDs enter a stack-free park loop before Rust,
logging, or shared state can be touched.

The linked ELF proves that the two MiB array ends exactly at `sbss`, so normal
BSS clearing begins after every slot. CPU0 computes the same stack top as the
old single-slot implementation. The entry retains CPU ID in `$a0` for the
future BSP/AP split.

This file is selected only for `board_laqemu`. The 2K1000LA assembly entry and
its single-core contract are unchanged. This batch does not make an AP online,
split `rust_main()`, change bootstrap ownership, or touch traps, scheduling,
locks, atomics, IPI, MM/TLB, filesystems, networking, or drivers.

## Validation summary

| Check | Result |
|---|---|
| Pre-change ELF witness | RED: one 0x40000 stack and SP assigned before CPUID |
| RV64 `CORE_NUM=1` kernel build | PASS, exit 0 |
| LA64 `CORE_NUM=1` kernel build | PASS, exit 0 |
| LA64 `CORE_NUM=2` focused kernel build | PASS, exit 0 |
| Full LA64 stack-array size | PASS, `0x200000` |
| Array upper bound equals `sbss` | PASS |
| Eight slot intervals | PASS, contiguous/disjoint/bounded |
| Entry instruction pattern/order | PASS |
| LA64 one-core waitqueue ktest | PASS, 4/4 |
| LA64 explicit two-core MTTCG waitqueue ktest | PASS, 4/4 |
| Two-core QEMU exit and image integrity | PASS, QEMU 0 / e2fsck 0 |
| QEMU thread snapshot | PASS, main thread plus four additional threads |
| Generated linker/lang-item selections restored | PASS |
| Evidence patches reverse dry-run | PASS |
| Diff whitespace check | PASS |
| Evidence freshness | PASS |
| AP online and 4/8-core runtime | NOT RUN by design |

The two-core command is recorded verbatim as
`-accel tcg,thread=multi -smp cpus=2,sockets=1,cores=2,threads=1`. The passing
tests execute on CPU0 because the existing LA64 bootstrap still parks nonzero
CPUs. This result proves that a two-vCPU topology does not regress CPU0 and
that the new entry layout is executable; it is deliberately not presented as
AP-online evidence.

## Evidence-discipline notes

An early ELF assertion script could false-pass because a failed shell test in
a piped group did not determine the group's exit status, and it initially read
the wrong `nm` field. The final assertions are fail-closed and explicitly
check every interval and instruction relation.

The first QEMU thread capture was overwritten by later output. A second
capture attempt contained an `awk` escaping error, terminated QEMU
fail-closed, and is preserved with a nonzero status. The final capture resolves
the QEMU child with `ps --ppid`, saves the snapshot separately, and passes.
After a late source-comment refinement, both builds, the layout audit, and
both QEMU runs were repeated; only `*-final.*` artifacts are acceptance
evidence.

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

- `smp-P1-B02-red-before.txt`: pre-change ELF symbols and `_start`
- `smp-P1-B02-*-build-final.log`: final serial dual-architecture and two-core
  focused build logs
- `smp-P1-B02-layout-after-final.txt`: final symbols and disassembly
- `smp-P1-B02-layout-check-final.txt`: fail-closed arithmetic/instruction
  assertions and all eight slot intervals
- `smp-P1-B02-la64-qemu-1core-final-output.log`: complete final one-core run
- `smp-P1-B02-la64-qemu-2core-final-output.log`: complete final explicit
  two-core MTTCG run
- `smp-P1-B02-la64-qemu-2core-final-threads.txt`: QEMU process/thread witness
- `smp-P1-B02-la64-qemu-judge-final.txt`: result classification and AP caveat
- `smp-P1-B02-la64-qemu-2core-thread-capture-attempt2-fail.*`: preserved
  fail-closed harness attempt
- `smp-P1-B02-diff-numstat.txt`, `smp-P1-B02-diff-u0.patch`: raw source diff
- `smp-P1-B02-line-ledger.md`: semantic critical-line classification
- `smp-P1-B02-doc-sync.patch`: architecture/plan synchronization
- `smp-P1-B02-container.txt`: container, image, and QEMU metadata
- `smp-P1-B02-freshness.txt`: source/evidence timestamp proof
- `smp-P1-B02-git-*.txt`: baseline and compact post-batch worktree state
- `smp-P1-B02-final-audit.*`: aggregate status, restoration, and whitespace
  audit
