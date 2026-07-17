# Structural A/B stop record

Stopped on 2026-07-16 at the user's request. No optimization was started.

Completed:

- adjacent production board run: nbody, string, float;
- adjacent `perf_diag stats_on=0` board run: nbody, string, float;
- adjacent diagnostic float `stats_on=1`, core counters limited to benchmark body;
- image, ELF, Cargo feature, initramfs and benchmark file identity checks;
- all suite/work/tmp paths verified on `/persist` ext4; runtime verified on `/tools` ext4 ro.

Key diagnostic float result:

- elapsed `72.0801748 s`, user `21.895004 s`, sys `50.070068 s`;
- `3,000,039` user unaligned traps;
- handler `4,767,941,219` ticks at 100 MHz = `47.67941219 s` = `95.22%` of sys;
- same diagnostic binary stats-off `72.49240693 s`; runtime stats delta `-0.57%`.

Not completed:

- CPUCFG cache geometry probe: deployment attempt timed out before completion; no result is claimed;
- PMU cache-miss measurements;
- additional production reset/repetition;
- Python explicit-munmap impact, ext4 phase isolation and mixed stability loop.

The raw serial logs and `records.jsonl` are immutable evidence. See `reports/structural_ab.csv` and `docs/09_debug/python-performance-checkpoint-20260716.md` for derived conclusions and evidence boundaries.
