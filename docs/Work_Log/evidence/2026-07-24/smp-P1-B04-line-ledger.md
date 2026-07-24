# SMP-P1-B04 Line Ledger

| File | Raw diff | Critical implementation |
|---|---:|---:|
| `os/src/smp.rs` | `+44/-1` | 34 |
| `os/src/hal/arch/riscv/mod.rs` | `+19/-0` | 12 |
| `os/src/hal/arch/loongarch64/mod.rs` | `+19/-0` | 12 |
| `os/src/hal/arch/mod.rs` | `+4/-4` | 8 |
| `os/src/hal/mod.rs` | `+2/-1` | 3 |
| **Total** | **`+88/-6`** | **69** |

The remaining added lines are direct safety/boundary comments and spacing.
The user approved completing this register-anchor invariant as one batch and
allowed a small overrun beyond the approximate 50-line guideline.
