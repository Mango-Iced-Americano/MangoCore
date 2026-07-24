# SMP-P0-B01 line ledger

Counting basis: `git diff --numstat` plus manual classification of every
line in `git diff --unified=0`. A changed production/configuration line counts
once as a deletion and once as an addition.

| File | Raw + | Raw - | Critical + | Critical - | Comments/docs + | Comments/docs - | Blank + |
|---|---:|---:|---:|---:|---:|---:|---:|
| `os/build.rs` | 13 | 2 | 8 | 0 | 4 | 2 | 1 |
| `os/make/rv64.mk` | 9 | 1 | 6 | 1 | 3 | 0 | 0 |
| `os/make/la64.mk` | 9 | 1 | 6 | 1 | 3 | 0 | 0 |
| **Total** | **31** | **4** | **20** | **2** | **10** | **2** | **1** |

Semantic critical count: **22**.

Classification notes:

- `build.rs` rustdoc and the two adjacent rationale comments are non-critical
  documentation; the environment tracking, validation, and rustc forwarding
  statements are critical.
- Each Makefile has three pure rationale comments. `CORE_NUM`, the allow-list,
  parse-time error, and exported environment assignment are critical.
- There are no test-code lines in the source patch. Evidence and Work Log files
  are documentation and excluded from the critical-code budget.
- Unclassified lines: **0**.
