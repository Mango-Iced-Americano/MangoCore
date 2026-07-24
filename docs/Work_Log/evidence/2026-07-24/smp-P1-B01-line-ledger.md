# SMP-P1-B01 critical-line ledger

The approved batch is limited to the RV64 pre-Rust boot-stack invariant.
Documentation, Work Log, evidence files, comments, and blank lines are not
counted as critical implementation code.

| Critical change | Added | Deleted | Purpose |
|---|---:|---:|---|
| `MAX_CPUS` and stack-size constants | 3 | 0 | Fix the eight-slot layout and 256 KiB stride |
| Pre-stack hart-ID guard | 2 | 0 | Reject IDs outside `0..8` before touching a stack |
| Per-hart stack calculation | 4 | 1 | Replace the shared stack top with `(hart + 1) << 18` |
| Invalid-hart/unexpected-return park | 3 | 0 | Avoid Rust, logging, or shared state without a safe slot |
| Stack-array alignment | 1 | 0 | Keep the array page aligned |
| Eight-slot storage and upper-bound symbol | 4 | 1 | Replace one slot with eight while preserving CPU0's top |
| **Total critical implementation** | **17** | **2** | **19 changed lines** |

Raw diff classification:

| Class | Added | Deleted |
|---|---:|---:|
| Critical implementation | 17 | 2 |
| Direct invariant comments | 14 | 0 |
| Blank lines | 4 | 0 |
| **Raw diff** | **35** | **2** |

No lock, atomic ordering, `unsafe` block, MM/TLB operation, scheduler state, or
runtime Rust interface was added. `a0/a1` remain unchanged across the entry
stack selection so a later BSP/AP split can consume the firmware arguments.
