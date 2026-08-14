# RV task-mapping deferred flush fixed-window A/B

- Date: `2026-08-14`
- Branch: `rv-satp-fix`
- Container: `rv-satp-fix-worktree-os-dev-1`
- Image: `zhouzhouyi/os-contest:20260510`
- Kernel SHA-256: `03299a249f1de33cae4568b956f1dceba8d554e5111191abdb9e58a65e8709a3`
- Golden SHA-256: `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`
- QEMU: 10.0.2, RV virt, MTTCG, 8 vCPU, 8 GiB
- Stats profile: `core_memory_io`, schema v7
- Window: 300 seconds from `BUILDSTORM_BEGIN`
- Final pair order: `deferred` then `eager`
- deferred log SHA-256: `fa6d1cb29bdf557c0c2d3e0f6f755490fab6993a0e085ba11ddfcdd3c2b06c5e`
- eager log SHA-256: `c718c0d03985b232525c54516ff20825beea745b79e28cee7a3d4d8689516256`

Both variants used the same diagnostic Image and x1 tools disk. Each run recreated the qcow2 x0
overlay from the same immutable official golden. The only runtime difference was
`mango.rv.kernel_task_sync=deferred` versus `eager`; `mango.rv.trap_return_fence_i=on` remained
fixed. The final pair reversed the prototype pair's order.

| metric | eager | deferred |
|---|---:|---:|
| final heavy sample | 78 | 78 |
| task publish sync/request | 273 | 275 |
| remote kernel full calls | 273 | 0 |
| remote TLB sync ticks total | 174,726,435 | 13,850,281 |
| remote TLB sync max ticks | 28,425,146 | 1,465,622 |
| task publish wait ticks | 160,886,163 | 0 |
| deferred local flushes | 0 | 250 |
| deferred local flush ticks | 0 | 153,118 |
| deferred local flush max ticks | 0 | 7,525 |
| task migration deferred requests | 0 | 0 |
| task run-slice ticks | 4,959,735,107 | 5,053,437,799 |
| parallel peak offset | 178 s | 168 s |
| last crate | `ax-posix-api` | `ax-posix-api` |
| shootdown failures | 0 | 0 |

At the exported 10 MHz clock, remote synchronization fell by 16.088 CPU-seconds (92.073%).
Including the deferred target-local flush cost, total measured TLB synchronization still fell by
91.986%. The maximum remote wait fell by 94.844%. Deferred coalesced 275 publication requests into
250 local flushes; they cost 15.312 ms total and 0.753 ms maximum. Task run-slice time increased by
9.370 CPU-seconds (1.889%), and the detected parallel phase began 10 seconds earlier.

Both windows passed preclean, toolchain and minibuild handshakes. Neither completed the full
BuildStorm workload within the diagnostic window; both reached the same last crate. There was no
kernel panic, unexpected exception, shootdown timeout, invalid stats snapshot, guest build failure,
or non-zero shootdown failure. These fixed-window results establish protocol cost and stage progress,
not a completed BuildStorm score.
