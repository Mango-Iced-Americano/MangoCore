# ext4_lwext4 migration evidence manifest

This directory contains the persistent evidence for the dirty
`board-develop-combined@78dd1c8c` working tree tested on 2026-07-18/19 +08:00.

## Accepted results

| Gate | Semantic result | Process result | Persistent evidence |
|---|---:|---:|---|
| RV64 regression | 6/6 top-level; namespace 9/9; sparse/truncate Phase 6 PASS | exit 0 | `rv64-regression.log`, `.status`, `.img`, `.img.fsck.log` |
| RV64 all ktest | 21/21; ext4 7/7 | exit 0 | `rv64-ktest.log`, `.img`, `.img.fsck.log` |
| LA64 regression | 6/6 top-level; namespace 9/9; sparse/truncate Phase 6 PASS | exit 0 | `la64-regression.log`, `.status`, `.img`, `.img.fsck.log` |
| LA64 all ktest | 20/21; all ext4 tests passed; `timer::tick_advances` sampled `t1 == t0` after 1 ms | exit 0 | `la64-ktest.log`, `.img`, `.img.fsck.log` |
| LA64 ext4-only ktest | 7/7 | exit 0 | `la64-ktest-ext4.log`, `.img`, `.img.fsck.log` |
| RV64 kernel build-only | release build completed | exit 0 | `rv64-build-only.log`, `.container-id` |
| LA64 kernel build-only | release build completed | exit 0 | `la64-build-only.log`, `.container-id` |
| Offline consistency | all five images completed e2fsck passes 1-5; each reports 11/16384 files and 1041/16384 blocks | exit 0 | five `*.img.fsck.log` files |

`qemu-output.log` concatenates the five complete retained test logs after only
removing NUL padding. `qemu-head-tail.txt` contains a 40-line head and tail for
each source log. The per-run files remain canonical because they preserve run
boundaries and original ANSI serial output.

## RED and discarded evidence

- `rv64-regression-debug.*` is intentionally retained RED evidence from before
  the sparse-extent deletion fix. Its container exited 2 and its image must not
  be confused with the accepted `rv64-regression.img`.
- Container `1831fc...` was an interrupted LA64 infrastructure attempt affected
  by a Rosetta/rustc stall. It is not a code-test result and has no accepted log.
- The LA64 all-ktest process exited 0 even though the TAP stream says 20/21.
  Semantic parsing therefore takes precedence over the container exit code.

## Evidence limitations

- The first five QEMU containers used `docker run --rm`; post-run
  `docker inspect` was impossible. Docker create/die events retained the full
  IDs, exit codes, durations, image, and host-to-container mount mapping; those
  fields are recorded in `container-id.txt`.
- Their literal original launcher argv was not retained. `commands.txt` records
  source-equivalent reproduction commands, while the complete internal QEMU
  argv remains in the tested Makefiles.
- These fixtures explicitly disable `has_journal`. Clean normal-shutdown fsck
  proves cold metadata consistency for these workloads, not crash consistency,
  journal replay, or orphan recovery.
- No physical SSD or board device was exposed to the containers.

See `git-hash.txt`, `config.txt`, `commands.txt`, `container-id.txt`, and
`freshness.txt` for the remaining required metadata. `sha256sums.txt` pins the
accepted logs, fixtures, build logs, combined output, and head/tail extract.
