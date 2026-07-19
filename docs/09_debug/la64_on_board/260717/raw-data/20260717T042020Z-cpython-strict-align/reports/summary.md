# MangoCore performance run summary

Generated: 2026-07-17T05:39:05.378475Z

| test | arch | platform | build | cache | n | median s | min s | max s | CV % |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| cpython_bench_bytesio | la64 | 2k1000la | diag_on | warm-runtime | 1 | 25.687506 | 25.687506 | 25.687506 | n/a |
| cpython_bench_chaos | la64 | 2k1000la | diag_on | warm-runtime | 1 | 43.349933 | 43.349933 | 43.349933 | n/a |
| cpython_bench_decimal | la64 | 2k1000la | diag_on | warm-runtime | 1 | 44.281234 | 44.281234 | 44.281234 | n/a |
| cpython_bench_dict | la64 | 2k1000la | diag_on | warm-runtime | 1 | 26.608110 | 26.608110 | 26.608110 | n/a |
| cpython_bench_fileio | la64 | 2k1000la | diag_on | warm-runtime | 1 | 100.645605 | 100.645605 | 100.645605 | n/a |
| cpython_bench_float | la64 | 2k1000la | diag_on | warm-runtime | 1 | 34.817481 | 34.817481 | 34.817481 | n/a |
| cpython_bench_fork | la64 | 2k1000la | diag_on | warm-runtime | 1 | 91.346158 | 91.346158 | 91.346158 | n/a |
| cpython_bench_hash | la64 | 2k1000la | diag_on | warm-runtime | 1 | 21.009016 | 21.009016 | 21.009016 | n/a |
| cpython_bench_json_loads | la64 | 2k1000la | diag_on | warm-runtime | 1 | 176.318861 | 176.318861 | 176.318861 | n/a |
| cpython_bench_list | la64 | 2k1000la | diag_on | warm-runtime | 1 | 56.098302 | 56.098302 | 56.098302 | n/a |
| cpython_bench_nbody | la64 | 2k1000la | diag_on | warm-runtime | 1 | 29.263873 | 29.263873 | 29.263873 | n/a |
| cpython_bench_pidigits | la64 | 2k1000la | diag_on | warm-runtime | 1 | 23.387621 | 23.387621 | 23.387621 | n/a |
| cpython_bench_regex | la64 | 2k1000la | diag_on | warm-runtime | 1 | 23.244687 | 23.244687 | 23.244687 | n/a |
| cpython_bench_richards | la64 | 2k1000la | diag_on | warm-runtime | 1 | 19.212099 | 19.212099 | 19.212099 | n/a |
| cpython_bench_sort | la64 | 2k1000la | diag_on | warm-runtime | 1 | 33.092459 | 33.092459 | 33.092459 | n/a |
| cpython_bench_spectral_norm | la64 | 2k1000la | diag_on | warm-runtime | 1 | 31.838549 | 31.838549 | 31.838549 | n/a |
| cpython_bench_string | la64 | 2k1000la | diag_on | warm-runtime | 1 | 17.302224 | 17.302224 | 17.302224 | n/a |
| cpython_bench_thread | la64 | 2k1000la | diag_on | warm-runtime | 1 | 65.720479 | 65.720479 | 65.720479 | n/a |
| scl | la64 | 2k1000la | diag_on | deployment | 1 | 0.041215 | 0.041215 | 0.041215 | n/a |
| sni | la64 | 2k1000la | diag_on | deployment | 1 | 0.158793 | 0.158793 | 0.158793 | n/a |
| snp | la64 | 2k1000la | diag_on | deployment | 1 | 0.125390 | 0.125390 | 0.125390 | n/a |
| sns | la64 | 2k1000la | diag_on | deployment | 1 | 83.143177 | 83.143177 | 83.143177 | n/a |
| spf | la64 | 2k1000la | diag_on | postflight | 1 | 0.416640 | 0.416640 | 0.416640 | n/a |
| spu | la64 | 2k1000la | diag_on | deployment | 1 | 1.519352 | 1.519352 | 1.519352 | n/a |
| ssm | la64 | 2k1000la | diag_on | deployment | 1 | 6.984319 | 6.984319 | 6.984319 | n/a |
| strict_ext4_preflight | la64 | 2k1000la | diag_on | booted-ext4 | 1 | 0.244882 | 0.244882 | 0.244882 | n/a |
| strict_functional_l3_l9 | la64 | 2k1000la | diag_on | strict-runtime | 1 | 44.650267 | 44.650267 | 44.650267 | n/a |
| strict_p4_mount | la64 | 2k1000la | diag_on | deployment | 2 | 0.078629 | 0.078559 | 0.078699 | 0.09 |
| strict_p4_parent | la64 | 2k1000la | diag_on | deployment | 2 | 0.079194 | 0.078527 | 0.079861 | 0.84 |
| strict_runtime_download | la64 | 2k1000la | diag_on | deployment | 2 | 37.904914 | 37.712319 | 38.097508 | 0.51 |
| strict_runtime_prepare | la64 | 2k1000la | diag_on | deployment | 2 | 0.111958 | 0.111330 | 0.112587 | 0.56 |
| strict_runtime_sha256 | la64 | 2k1000la | diag_on | deployment | 2 | 82.969094 | 82.958099 | 82.980089 | 0.01 |
| strict_suite_preflight | la64 | 2k1000la | diag_on | ext4-existing-suite | 1 | 23.297531 | 23.297531 | 23.297531 | n/a |
