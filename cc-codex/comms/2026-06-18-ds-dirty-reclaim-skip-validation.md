# Codex -> DS：dirty/event-driven reclaim skip 验证任务

目标：验证第二阶段修复是否消除 S2b 中 `prune_children_stale_entries_budgeted` 的反复空扫/近空扫成本，同时确认 incremental prune 已解决的长尾尖刺不回退。

本任务只要求 DS 做实验、解析、报告。不要修改核心逻辑。

## 1. 背景结论

`cc-codex/results-20260618-inc-prune/` 的复测说明：

- P0 通过：S1 `prune_kids cycles_max` 从 780M 降到 103M，长尾尖刺已被 incremental/cursor prune 明显分摊。
- 旧 ext4 目录 O(n) 没有复发：`dir_full_scan_count=0`。
- S2b 仍重：raw musl 中 `Simple open/close=3126us`、`Pipe latency=4212.85us`、lmbench group `113s`。
- S2b 的关键矛盾不是大量 stale 被清掉，而是空扫税：raw musl 中 `kids_removed=4`，但 `prune_kids cycles_total=4,406,546,434`、`calls=4188`。

注意：`results-20260618-inc-prune/report.md` 有部分表格值与 raw 不一致，例如 S1 musl raw 为 `group 107s`、`open/close 1089.2us`，报告表写成 `71s`、`953us`。本轮请以 raw log 解析结果为准，不要手抄旧 report 表格。

## 2. 本次代码变化

### `os/src/fs/ext4/ext4fs.rs`

- `Ext4BudgetPruneStats` / `Ext4ChildrenBudgetPruneStats` 增加 `skipped`。
- `Ext4FileSystem` 增加 dirty generation：
  - `inode_objects_prune_gen`
  - `inode_objects_pruned_gen`
  - `children_prune_gen`
  - `children_pruned_gen`
- `insert_inode_object`、`canonical_inode_object`、`remove_inode_object`、lookup stale invalidation 会标记 inode-object prune pending。
- `find/create/create_with_attrs/symlink/rename/link/get_entry_name` 中 children cache 插入、移动或 stale invalidation 会标记 children prune pending。
- `prune_inode_objects_budgeted(max_entries, force)`：
  - 非 force 且 generation 已追平时直接返回 `skipped=true`。
  - 完成一个 cursor pass 后更新 `inode_objects_pruned_gen`。
  - 保留 force 模式用于 heap pressure/critical。
- `prune_children_stale_entries_budgeted(max_parent_inodes, max_child_entries, force)`：
  - 非 force 且 generation 已追平时直接返回 `skipped=true`。
  - 完成一个 parent cursor pass 后更新 `children_pruned_gen`。
  - 保留 force 模式用于 heap pressure/critical。

设计含义：normal reclaim 不再每 64 tick 固定扫 ext4 weak cache；只有 cache 结构发生变更后才做一轮 budgeted cleanup。Weak 对象自然过期本身没有回调，所以 heap pressure/critical 仍会 force 扫描，避免长期内存压力下 stale Weak 永久堆积。

### `os/src/fs/reclaim.rs`

- normal reclaim 调用 budgeted prune 时传 `force=false`。
- heap pressure / critical 时传 `force=true`，并使用更大的预算。
- `reclaim_budget` 输出新增两个字段：

```text
reclaim_budget io_scanned=... io_budget_hit=... io_skipped=... kids_parents_scanned=... kids_entries_scanned=... kids_budget_hit=... kids_skipped=...
```

## 3. 必跑矩阵

先只跑 rv64，一轮即可：

1. S0：`cc-codex/scenario-s0-lmbench-only.conf`
2. S1：`cc-codex/scenario-s1-short-pollution.conf`
3. S2b：`cc-codex/scenario-s2b-libcbench-only.conf`

如果三项都有效，再补：

4. S2a：`cc-codex/scenario-s2a-iozone-only.conf`
5. F5：full order 到 lmbench

每个 scenario 前必须恢复 clean image。不要复用上一个 scenario 的污染镜像。

## 4. 必须解析字段

### lmbench

- group time
- `Simple read`
- `Simple write`
- `Simple stat`
- `Simple fstat`
- `Simple open/close`
- `Pipe latency`
- `Pipe bandwidth`
- `Process fork+exit`
- `Process fork+execve`
- `bw_file_rd io_only`
- `bw_file_rd open2close`
- context switch 全部行

### reclaim

- `reclaim calls/runs/cycles_total/cycles_max`
- `io_removed/pc_removed/kids_removed/clean_freed/cached_pages_max/heap_pressure_runs/heap_critical_runs`
- `reclaim_stage_prune_io calls/cycles_total/cycles_max`
- `reclaim_stage_prune_pc calls/cycles_total/cycles_max`
- `reclaim_stage_prune_kids calls/cycles_total/cycles_max`
- `reclaim_stage_cache_metric calls/cycles_total/cycles_max`
- `reclaim_stage_shrink calls/cycles_total/cycles_max`

### budget/skip

必须解析完整 `reclaim_budget`：

- `io_scanned`
- `io_budget_hit`
- `io_skipped`
- `kids_parents_scanned`
- `kids_entries_scanned`
- `kids_budget_hit`
- `kids_skipped`

### ext4 counters

- `dir_full_scan_count`
- `dir_full_scan_entries`
- `dir_cache_linear_scan`
- `dir_cache_scanned_entries`
- `dentry_lookup_count`
- `dentry_cache_hit`
- `dentry_cache_miss`
- `children stale_weak`
- `inode_cache hit/miss`

## 5. 判定标准

### P0：不能回退

- S1 `prune_kids cycles_max < 120M`，不能回到 780M 级别。
- S1 `Simple open/close`、`Simple stat`、`Pipe latency` 不得比 incremental prune raw 明显更差。
- `dir_full_scan_count=0`。
- 无 panic。

### P1：验证 dirty skip 是否生效

- S2b `kids_skipped > 0`，且应占 reclaim run 的明显比例。
- S2b `kids_parents_scanned` 与 `kids_entries_scanned` 相比 incremental prune 明显下降。
- S2b `prune_kids cycles_total` 应明显低于 incremental prune raw 的 4.4B 级别。
- S2b `kids_removed` 仍可能很低；这不是失败。关键是低 removed 时不应继续付出高 `cycles_total`。

### P2：用户态指标

和 incremental prune raw 对比：

- S2b musl `open/close` 目标：明显低于 `3126us`。
- S2b musl `pipe latency` 目标：明显低于 `4212us`。
- S2b musl group time 目标：明显低于 `113s`。
- S1 musl raw 基线使用 `open/close 1089.2us`、`stat 656us`、`pipe 3398us`、group `107s`。

## 6. 失败信号

任一项出现请标红：

- `prune_kids cycles_max > 200M`
- `dir_full_scan_count > 0`
- `io_skipped/kids_skipped` 长期为 0，说明 dirty generation 没有生效
- `kids_removed` 接近 0，但 `prune_kids cycles_total` 仍是数十亿 cycles
- S2b 比 incremental prune raw 更差
- 任何 panic 或 lmbench profile marker 缺失

## 7. 交付路径

输出到：

```text
cc-codex/results-20260618-dirty-reclaim-skip/
```

目录结构：

```text
raw/
  dirty-rv64-S0-round1.log
  dirty-rv64-S1-round1.log
  dirty-rv64-S2b-round1.log
  dirty-rv64-S2a-round1.log   # 若执行
  dirty-rv64-F5-round1.log    # 若执行
parsed/
  lmbench-metrics.json
  reclaim-profiles.json
  ext4-profiles.json
  combined.csv
patches/
  head.diff
report.md
manifest.json
```

`report.md` 必须包含：

- 样本有效性表。
- S0/S1/S2b 与 incremental prune raw 的对比表。
- `io_skipped/kids_skipped` 与 `runs` 的比例。
- `kids_removed` vs `prune_kids cycles_total` 的解释。
- 对 P0/P1/P2 的逐项判定。
