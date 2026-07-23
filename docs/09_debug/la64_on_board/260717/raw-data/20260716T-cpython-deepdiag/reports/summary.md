# MangoCore performance run summary

Generated: 2026-07-16T10:29:15.284754Z

| test | arch | platform | build | cache | n | median s | min s | max s | CV % |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| clock_sleep_shell | la64 | 2k1000la | diag_off | n/a | 1 | 1.039245 | 1.039245 | 1.039245 | n/a |
| cpybench_deploy | la64 | 2k1000la | diag_off | deployment | 9 | 28.671700 | 28.496743 | 29.808436 | 1.58 |
| cpybench_sha256 | la64 | 2k1000la | diag_off | deployment | 9 | 1.905888 | 1.901664 | 1.935516 | 0.51 |
| cpybench_verify | la64 | 2k1000la | diag_off | deployment | 9 | 0.150238 | 0.149471 | 0.153150 | 0.94 |
| diag_features | la64 | 2k1000la | diag_off | unspecified | 1 | 0.124815 | 0.124815 | 0.124815 | n/a |
| ext4_fileio_scaled_core | la64 | 2k1000la | diag_on | ext4-freshdir | 1 | 28.274384 | 28.274384 | 28.274384 | n/a |
| ext4_fileio_scaled_memory | la64 | 2k1000la | diag_on | ext4-freshdir | 1 | 28.246356 | 28.246356 | 28.246356 | n/a |
| ext4_fileio_scaled_off | la64 | 2k1000la | diag_off | ext4-freshdir | 1 | 27.889474 | 27.889474 | 27.889474 | n/a |
| ext4_float_core_class | la64 | 2k1000la | diag_on | ext4-warm | 1 | 97.811481 | 97.811481 | 97.811481 | n/a |
| ext4_math_function_attribution | la64 | 2k1000la | diag_on | ext4-runtime | 2 | 12.365935 | 8.784434 | 15.947435 | 28.96 |
| ext4_mmap_release_16m | la64 | 2k1000la | diag_on | resident-private | 1 | 8.367341 | 8.367341 | 8.367341 | n/a |
| ext4_mmap_release_1m | la64 | 2k1000la | diag_on | resident-private | 1 | 8.006189 | 8.006189 | 8.006189 | n/a |
| ext4_mmap_release_32m | la64 | 2k1000la | diag_on | resident-private | 1 | 9.211547 | 9.211547 | 9.211547 | n/a |
| ext4_mmap_release_4m | la64 | 2k1000la | diag_on | resident-private | 1 | 8.049337 | 8.049337 | 8.049337 | n/a |
| ext4_mmap_release_64m | la64 | 2k1000la | diag_on | resident-private | 1 | 12.437670 | 12.437670 | 12.437670 | n/a |
| ext4_mount_probe | la64 | 2k1000la | diag_off | n/a | 1 | 0.895097 | 0.895097 | 0.895097 | n/a |
| ext4_short_paths | la64 | 2k1000la | diag_off | ext4 | 1 | 0.161901 | 0.161901 | 0.161901 | n/a |
| ext4_string_core_class | la64 | 2k1000la | diag_on | ext4-warm | 1 | 38.687359 | 38.687359 | 38.687359 | n/a |
| ext4_string_memory | la64 | 2k1000la | diag_on | ext4-warm | 1 | 38.911151 | 38.911151 | 38.911151 | n/a |
| float_body_core | la64 | 2k1000la | diag_on | warm-runtime | 1 | 98.407900 | 98.407900 | 98.407900 | n/a |
| nb_on | la64 | 2k1000la | diag_on | warm-runtime | 1 | 35.343258 | 35.343258 | 35.343258 | n/a |
| nbody_body_core | la64 | 2k1000la | diag_on | warm-runtime | 1 | 35.334718 | 35.334718 | 35.334718 | n/a |
| nbody_diag_off | la64 | 2k1000la | diag_off | warm-runtime | 1 | 35.161062 | 35.161062 | 35.161062 | n/a |
| scratch_post_reboot_audit | la64 | 2k1000la | diag_off | unspecified | 1 | 0.187387 | 0.187387 | 0.187387 | n/a |
| scratch_redeploy_sync | la64 | 2k1000la | diag_off | unspecified | 1 | 0.671940 | 0.671940 | 0.671940 | n/a |
| string_body_core | la64 | 2k1000la | diag_on | warm-runtime | 1 | 38.903196 | 38.903196 | 38.903196 | n/a |
| string_body_off | la64 | 2k1000la | diag_off | warm-runtime | 1 | 38.415608 | 38.415608 | 38.415608 | n/a |
