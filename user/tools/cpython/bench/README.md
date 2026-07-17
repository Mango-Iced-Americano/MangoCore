# MangoCore CPython benchmarks

The 18 `bm_*.py` workloads are derived from
[`Mango-Iced-Americano/cpython-tests`](https://github.com/Mango-Iced-Americano/cpython-tests)
at commit `c50669c2b59a7d6d979fb12aea42c1b508ed3765`.

The source repository's `pidigits` produced variable-width values instead of
2000 decimal digits, and its `richards` workload severed the task chain during
the first scheduler pass. Those two algorithms are replaced with validated
implementations adapted from `python/pyperformance` commit
`216cbeb5f828b8ee5864f9bb52f3563d2d1a4846`; see
`PYPERFORMANCE_LICENSE.txt`.

MangoCore keeps the workload bodies recognizable while using a local runner
that records every sample.  The board entry point is
`../cpython_benchmark.sh`; it runs each module in a fresh CPython process,
defaults to one warmup plus one measured sample, enforces a per-module
timeout, and writes temporary data only below the selected scratch directory.

Lines prefixed with `CPYTHON_BENCH_JSON ` are stable JSON records consumed by
`scripts/kernel_perf.py`.  The runner intentionally does not emit an aggregate
geometric mean because these workloads use different units of work.

For a `perf_diag` attribution run, set `CPYTHON_BENCH_TARGET_STATS=1`.  The
runner then keeps counters off during interpreter startup, imports and warmups,
resets them immediately before each measured `benchmark()` call, and freezes
them immediately afterwards.  This mode is diagnostic only and does not change
the production benchmark defaults.

`bm_fileio` additionally accepts `CPYTHON_FILEIO_CHUNK_COUNT`,
`CPYTHON_FILEIO_SMALL_FILE_COUNT`, and `CPYTHON_FILEIO_DIRECT_COUNT` for short
counter-attribution probes.  Leaving them unset retains the formal 10 MiB,
5,000-small-file, and 50-direct-write workload.  The compact
`CPYTHON_FILEIO_PROFILE=diag-short` preset selects 64 chunks, 100 small files,
and four direct writes for serial command lines with a strict size limit.

`diag_mmap_release.py` is a diagnostic-only helper (not one of the 18 formal
benchmarks).  It makes every page of a private anonymous mapping resident and
then measures `munmap`; `MANGO_MMAP_MIB` selects the mapping size.  It gates
`perf_diag` counters around teardown only.

`diag_math_unaligned.py` is another diagnostic-only helper.  It runs
`sqrt/sin/cos/log/pow/exp` in separate reset windows and records the unaligned
trap classes for each function; `MANGO_MATH_LOOPS` controls the iteration count.
