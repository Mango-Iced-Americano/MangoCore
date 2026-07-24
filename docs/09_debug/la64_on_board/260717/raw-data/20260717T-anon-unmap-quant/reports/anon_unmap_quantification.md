# Anonymous VMA release quantification

Clock: 100000000 Hz. Counter time is measured inside `Vma::unmap`.

## Resident anonymous mapping microbenchmark

| MiB | pages | close ms | unmap ms | largest ms | scans | expected primary | extra |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 256 | 2.446 | 1.860 | 1.849 | 32899 | 32896 | 3 |
| 4 | 1024 | 18.336 | 17.657 | 17.646 | 524803 | 524800 | 3 |
| 16 | 4096 | 238.420 | 237.281 | 237.269 | 8390659 | 8390656 | 3 |
| 32 | 8192 | 959.497 | 958.383 | 958.368 | 33558531 | 33558528 | 3 |
| 64 | 16384 | 3889.679 | 3888.325 | 3888.306 | 134225923 | 134225920 | 3 |

## Strict-aligned CPython workloads

| benchmark | elapsed s | anonymous unmap s | body share | sys share | calls | resident pages | max active pages | scans | max call s |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| bm_list | 21.226 | 2.396336 | 11.290% | 29.143% | 436 | 110176 | 3909 | 70469395 | 0.327313 |
| bm_dict | 6.288 | 0.609509 | 9.693% | 46.901% | 56 | 22000 | 5121 | 19751552 | 0.370314 |
| bm_bytesio | 5.841 | 0.251081 | 4.298% | 18.236% | 146 | 21698 | 1223 | 6398530 | 0.024344 |
| bm_fork | 29.707 | 0.228320 | 0.769% | n/a | 11386 | 41231 | 235 | 1921022 | 0.001627 |
| bm_thread | 25.529 | 0.007704 | 0.030% | 0.917% | 1122 | 1322 | 252 | 33442 | 0.001881 |
| bm_json_loads | 84.956 | 0.000010 | 0.000% | 0.003% | 1 | 2 | 2 | 3 | 0.000010 |

The share is diagnostic attribution, not an optimized-runtime speedup prediction.
`bm_fork` sys share is n/a because its sample rusage is parent-only while kernel counters include children.
Warmup and setup are excluded by the target-side reset/on/off window.
