# SMP-P1-B02 critical-line ledger

The approved batch is limited to the LA64 QEMU pre-Rust boot-stack invariant.
Documentation, Work Log, evidence files, comments, and blank lines are not
counted as critical implementation code.

| Critical change | Added | Deleted | Purpose |
|---|---:|---:|---|
| `MAX_CPUS` ceiling | 1 | 0 | Fix the reserved topology at eight contiguous IDs |
| CPUID read and pre-stack guard | 3 | 1 | Move the existing read before stack selection and reject invalid IDs |
| CPU-local stack base and offset | 4 | 2 | Replace the shared slot with `(cpu_id + 1) * BOOT_STACK_SIZE` |
| Invalid-CPU park | 2 | 0 | Avoid Rust, logging, and shared state without a valid stack |
| Assembly `MAX_CPUS` operand | 1 | 0 | Keep the guard tied to the Rust constant |
| Eight-slot storage | 2 | 1 | Replace one 256 KiB slot with an eight-slot array |
| **Total critical implementation** | **13** | **4** | **17 changed lines** |

Raw diff classification:

| Class | Added | Deleted |
|---|---:|---:|
| Critical implementation | 13 | 4 |
| Direct startup-invariant comments | 11 | 7 |
| Blank lines | 3 | 0 |
| **Raw diff** | **27** | **11** |

The two-line Rust array declaration is counted by raw physical lines. No lock,
atomic ordering, MM/TLB operation, scheduler state, or new `unsafe` ownership
relationship was introduced. The existing naked entry function remains the
only unsafe context in this file.
