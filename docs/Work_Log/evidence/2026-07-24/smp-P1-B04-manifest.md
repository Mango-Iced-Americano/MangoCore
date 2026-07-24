# SMP-P1-B04 Evidence Manifest

Status: `partial`

The implementation was built and exercised in Docker before this manifest was
written:

- RV64 and LA64 release kernel builds with `CORE_NUM=1`: pass.
- RV64 and LA64 waitqueue ktest with `CORE_NUM=1/2/4/8`: pass.
- RV64 and LA64 competition-style `-smp 8` without explicit `-accel`: pass.
- Both 8-core runs reached `online_mask=0xff` and reported no panic.
- `PER_CPUS` is `0x200` bytes and 64-byte aligned in both ELF files.
- Disassembly shows the expected RV64 `tp` and LA64 `$r21` write/read pairs.

Runtime environment:

- Container: `lzm-cagent-run`
- Image: `zhouzhouyi/os-contest:20260104`
- RV64 QEMU: 10.0.2
- LA64 QEMU: 10.0.2

The user stopped a redundant evidence replay. Its incomplete log was deleted
instead of being mislabelled as a passing artifact. Full raw logs were
therefore not persisted in this directory, so the archive remains `partial`
even though the observed build and focused runtime checks passed.
