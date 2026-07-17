# Final diagnostic verification

All builds ran sequentially in Docker image `zhouzhouyi/os-contest:20260104`.

| architecture | kernel SHA-256 | build | QEMU diag smoke |
|---|---|---|---|
| rv64 | `a0ad8aad617400aa3112f0365b35304e7ea85a17a40c33b15862dae6dca2d416` | pass | pass |
| la64 | `7bd0dc97eb27144344477a18173ed8f69fecc0d1ce6096f3a966060b403ecb22` | pass | pass |

The QEMU smoke test checked that `/sys/kernel/stats` exists, profiles switch,
reset works, `stats_on=0` freezes `syscall_total`, and the
`network_runtime` window records `runtime_exec_calls=3`. Both consoles reached
`=== DIAG SMOKE TEST DONE ===` without `FAIL:` or panic.

QEMU fixture hashes:

- rv64 sdcard: `0da4a33b26b7a689699a1931cd5002cd08e9527226b94cf90138d221d1b41fa8`
- rv64 tools: `dfce5b20048080a37dcb958f77a6045663e3c65608ba6e82db558f8b0b51e7eb`
- la64 sdcard: `d68b690032947c5d942d9dc7f141696877d5a138ec4cee57804e66944ccd1bc7`
- la64 tools: `fc27b1748b6953e36df518909cfd69733ed161741269c6c56eeed97c01853f36`

The fixture images are reused from run `20260715T-mangocore-perf-baseline`;
the kernels above were rebuilt from the final 2026-07-16 diagnostic source.
QEMU is a functional gate only. Performance conclusions remain based on the
2K1000LA production board run.
