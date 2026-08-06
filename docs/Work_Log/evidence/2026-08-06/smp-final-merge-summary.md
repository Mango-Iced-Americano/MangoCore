# SMP final merge evidence

- Date: 2026-08-06
- Target branch: `smp`
- First parent: `origin/smp@952dddcce74582f2674cf22684b26b6c23137888`
- Integrated head: `codex/smp-develop-integration@eb2d7e53eba0355ba15e23230c00b72e55419dcc`
- Docker container: `mangocore-smp-final-merge-20260806-os-dev-1`
- CPU count: 8

## Conflict resolution

The integrated branch already contains the later `smp-fs-net` implementation and its follow-up
fixes. Conflicting production files and focused tests therefore use the integrated versions as
one coherent concurrency protocol. The final source/configuration tree matches `eb2d7e53`; the
additional tree delta consists only of documentation, plans, work logs, and reusable guidance
preserved from `origin/smp`.

## Independent review

Local DeepSeek/Claude-compatible read-only job `smp-final-merge-review-20260806` completed with
exit code 0, no source mutation, and no merge-blocking finding. Its only recommendation was to
document the normal-boot publication order of PID1 and the network poll worker; the document was
updated before final validation.

## Docker validation

| Job | Result | Duration |
|---|---:|---:|
| `smp-final-merge-rv-build-final-20260806` | PASS | 153.569 s |
| `smp-final-merge-la-build-20260806` | PASS | 175.592 s |
| `smp-final-merge-rv-smp-20260806` | PASS | 150.617 s |
| `smp-final-merge-la-smp-20260806` | PASS | 147.454 s |
| `smp-final-merge-rv-fs-smp-20260806` | PASS | 141.417 s |
| `smp-final-merge-la-fs-smp-20260806` | PASS | 140.674 s |
| `smp-final-merge-rv-net-smp-20260806` | PASS | 150.237 s |
| `smp-final-merge-la-net-smp-20260806` | PASS | 148.487 s |

All focused QEMU jobs reported `configured=8`, `online_mask=0xff`, and
`KTEST RESULT: PASS`. The deterministic runner found no forbidden marker and no source mutation.

Two earlier RV64 build jobs are intentionally not counted as source validation. The first stopped
at toolchain preflight because the fresh container had not provisioned `nightly-2026-05-10`; the
second stopped because the fresh worktree had not initialized the `buddy_system_allocator`
submodule. Both prerequisites were repaired through the repository-defined setup paths before the
PASS matrix above was run.
