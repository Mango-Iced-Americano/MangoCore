---
title: "LTP lwext4 per-case delta diagnostic"
category: testing
status: stable
owner: MangoCore Team
last_updated: 2026-07-17
code_paths:
  - "user/src/bin/ltprunner.rs"
  - "user/src/bin/ltprunner/lwext4_perf/mod.rs"
  - "user/src/bin/ltprunner/lwext4_perf/snapshot.rs"
related_docs:
  - "docs/ltp/ltp_workflow.md"
entry_points:
  - "ltp_lwext4_perf_log"
---

# LTP lwext4 per-case delta diagnostic

`ltprunner` can print a bounded, opt-in delta line for the cumulative
`/sys/kernel/stats/lwext4` counters. It is for correlating the selected
`gf14`, `gf18`, `gf27`, and `gf28` cases with lwext4 activity; it does not fix
a sparse-hole issue and never resets or writes kernel statistics.

## Enablement

Add exactly one configuration key:

```ini
ltp_lwext4_perf_log=1
```

The runner probes `/sys/kernel/stats/features` once only when the flag is true.
Collection starts only when that file contains `perf_diag=true`. With the key
absent or false, the runner performs no diagnostic sysfs reads and emits no
diagnostic output. This preserves existing execution, accounting, ordering, and
exit-status behavior.

For a focused suite run, inject `os_test.lwext4_perf.focus.conf`. Its
`ltp_include` filters to `gf14,gf18,gf27,gf28`, but does not reorder the
runtest suite: executed cases retain their runtest-file order. It leaves the
default `os_test.conf` unchanged.

## Collection rules

- The runner reads `/sys/kernel/stats/lwext4` directly with `open/read/close`.
- The fixed diagnostic sysfs paths are NUL-terminated for the existing user
  syscall ABI.
- Each executed case has one snapshot immediately before `run_case()` and one
  immediately after it returns.
- Every known counter must appear once as a decimal `key=value` field. Duplicate,
  missing, malformed, or overflowing known values disable collection.
- Unknown future fields are ignored.
- Each delta is `post.wrapping_sub(pre)` as an unsigned 64-bit value.
- After each executed case with successful pre/post snapshots, exactly one line
  is printed directly to the existing LTP/QEMU log:

  ```text
  [ltprunner] lwext4-perf case_index=42 exit_status=0 deltas=17,0,...
  ```

  `case_index` is the same index in the preceding
  `[ltprunner] #<index> suite=<...> case=<name>` line; no case name is copied
  or stored by the diagnostic. The comma-separated values are the 24 counters
  below in their listed order.
- If the feature probe, a pre-snapshot, or a post-snapshot fails, the runner
  prints exactly one `status=unavailable reason=<stable_reason>` line after the
  affected case and suppresses later diagnostic lines. This never changes LTP
  execution, accounting, ordering, or exit status.
- No sysfs control file is written, no counter is reset, and the diagnostic
  creates no files or report output.

## Delta order

The `reason` value is one of `feature_probe`, `before_snapshot_read`,
`before_snapshot_parse`, `after_snapshot_read`, or `after_snapshot_parse`.
`exit_status` is the unchanged numeric result returned by `run_case()`. All
delta values are unsigned decimal `u64` values in this fixed order:

1. `lwext4_find_calls`
2. `lwext4_find_cycles`
3. `lwext4_probe_type_calls`
4. `lwext4_probe_type_cycles`
5. `lwext4_get_inode_id_calls`
6. `lwext4_get_inode_id_enoint`
7. `lwext4_get_inode_id_cycles`
8. `lwext4_metadata_cold`
9. `lwext4_metadata_hot`
10. `lwext4_metadata_cold_cycles`
11. `lwext4_file_open_calls`
12. `lwext4_file_open_cycles`
13. `lwext4_file_size_calls`
14. `lwext4_file_close_calls`
15. `lwext4_file_close_cycles`
16. `lwext4_dir_entries_calls`
17. `lwext4_dir_entries_cycles`
18. `lwext4_create_pre_check`
19. `lwext4_logical_size_calls`
20. `lwext4_logical_size_cycles`
21. `lwext4_ensure_pc_calls`
22. `lwext4_find_cache_hit`
23. `lwext4_find_cache_miss`
24. `lwext4_ensure_pc_creates`

The `enoint` spelling is intentional: it is the current sysfs ABI key and
must remain unchanged for strict validation.
