# MangoCore performance run summary

Generated: 2026-07-18T00:23:10.758523Z

| test | arch | platform | build | cache | n | median s | min s | max s | CV % |
|---|---|---|---|---|---:|---:|---:|---:|---:|
| aligned_pillow_artifact_verify | la64 | docker | strict_aligned | warm | 1 | 41.284616 | 41.284616 | 41.284616 | n/a |
| aligned_pillow_p4_ext4_io | la64 | 2k1000la | production | warm | 1 | 6.070883 | 6.070883 | 6.070883 | n/a |
| aligned_pillow_qemu_smoke | la64 | qemu-user | strict_aligned | warm | 1 | 3.454839 | 3.454839 | 3.454839 | n/a |
| aligned_pillow_runtime_build | la64 | docker | strict_aligned | warm | 1 | 251.600821 | 251.600821 | 251.600821 | n/a |
| aligned_python_native_closure_build | la64 | docker | strict_aligned | warm | 1 | 191.687580 | 191.687580 | 191.687580 | n/a |
| aligned_python_pyyaml_build | la64 | host-docker | production | warm | 1 | 168.581404 | 168.581404 | 168.581404 | n/a |
| cpython_l3_l9_final | la64 | 2k1000la | production | warm | 1 | 49.045762 | 49.045762 | 49.045762 | n/a |
| final_la64_kernel_build | la64 | docker | production | warm | 1 | 91.543204 | 91.543204 | 91.543204 | n/a |
| final_rv64_kernel_build | rv64 | docker | production | warm | 1 | 83.360867 | 83.360867 | 83.360867 | n/a |
| la64_kernel_build_gate | la64 | docker | release | warm | 1 | 90.125796 | 90.125796 | 90.125796 | n/a |
| p4_failed_stage_cleanup | la64 | 2k1000la | production | deployment | 1 | 4.899977 | 4.899977 | 4.899977 | n/a |
| p4_final_prune | la64 | 2k1000la | production | deployment | 1 | 36.535432 | 36.535432 | 36.535432 | n/a |
| p4_final_state | la64 | 2k1000la | production | warm | 1 | 1.509723 | 1.509723 | 1.509723 | n/a |
| p4_old_release_manifest_audit | la64 | 2k1000la | production | deployment | 1 | 0.222936 | 0.222936 | 0.222936 | n/a |
| p4_old_release_prune | la64 | 2k1000la | production | deployment | 1 | 108.924218 | 108.924218 | 108.924218 | n/a |
| p4_post_prune_audit | la64 | 2k1000la | production | deployment | 1 | 1.649840 | 1.649840 | 1.649840 | n/a |
| p4_stale_part_cleanup | la64 | 2k1000la | production | deployment | 1 | 0.229359 | 0.229359 | 0.229359 | n/a |
| persist_default_gate | la64 | 2k1000la | production | warm | 1 | 137.598552 | 137.598552 | 137.598552 | n/a |
| pillow_ext4_smoke | la64 | 2k1000la | production | warm | 1 | 6.230044 | 6.230044 | 6.230044 | n/a |
| pyctl_get | la64 | 2k1000la | production | deployment | 5 | 0.071310 | 0.064849 | 0.075346 | 5.54 |
| pyctl_rm | la64 | 2k1000la | production | deployment | 3 | 0.038973 | 0.038665 | 0.040457 | 1.99 |
| pyctl_run | la64 | 2k1000la | production | deployment | 3 | 738.654277 | 715.133800 | 1160.473395 | 23.48 |
| pyctl_sha | la64 | 2k1000la | production | deployment | 5 | 0.047754 | 0.046652 | 0.048113 | 1.12 |
| rv64_kernel_build_gate | rv64 | docker | release | warm | 1 | 77.001212 | 77.001212 | 77.001212 | n/a |
| smolagent_agentimage | la64 | 2k1000la | production | warm | 1 | 27.201502 | 27.201502 | 27.201502 | n/a |
| smolagent_declared_requirements | la64 | 2k1000la | production | warm | 1 | 4.434047 | 4.434047 | 4.434047 | n/a |
| smolagent_dependency_metadata | la64 | 2k1000la | production | warm | 1 | 5.172242 | 5.172242 | 5.172242 | n/a |
| smolagent_direct_dependency_inventory | la64 | 2k1000la | production | warm | 1 | 17.634450 | 17.634450 | 17.634450 | n/a |
