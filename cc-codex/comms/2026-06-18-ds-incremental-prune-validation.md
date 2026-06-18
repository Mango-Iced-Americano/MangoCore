# Codex → DS：incremental prune 修复验证任务

目标：验证 Codex 新的 budgeted/incremental reclaim prune 是否解决 `prune_children_stale_entries` 的长尾尖刺，并观察 full-polluted lmbench 的多项退化是否恢复。

本任务只要求 DS 做实验、解析、报告。不要修改核心逻辑。

## 1. 本次代码变化

Codex 已在主工作区实现第一阶段修复：

### `os/src/fs/ext4/ext4fs.rs`

- 给每个 `Ext4FileSystem` 增加 `reclaim_cursor`。
- 新增 `prune_inode_objects_budgeted(max_entries)`：
  - 按 inode number cursor 增量扫描 `inode_objects`。
  - 每次最多扫描固定数量 entry。
  - 返回 `scanned / removed / budget_hit`。
- 新增 `prune_children_stale_entries_budgeted(max_parent_inodes, max_child_entries)`：
  - 按 `(parent_ino, child_name)` cursor 增量扫描目录 `children` weak cache。
  - 每次最多扫描固定数量 parent inode registry entry 和 child entry；parent 预算按 raw `inode_objects` entry 计数，不按 live parent 计数，避免大量 stale Weak 绕开预算。
  - 返回 `parents_scanned / entries_scanned / removed / budget_hit`。
- 原全量函数 `prune_inode_objects()` / `prune_children_stale_entries()` 保留，供 debug syscall/manual reclaim 使用。

### `os/src/fs/reclaim.rs`

- scheduler-loop reclaim 改用 budgeted prune。
- 不再使用“每 16 次全量 prune 一次”的 batching。
- 每个 reclaim run 都只处理小预算：
  - normal: inode `64`，children parent `8`，children entry `64`
  - heap pressure: inode `128`，children parent `16`，children entry `128`
  - heap critical: inode `256`，children parent `32`，children entry `256`
- `dump_reclaim_stats()` 新增一行：

```text
reclaim_budget io_scanned=... io_budget_hit=... kids_parents_scanned=... kids_entries_scanned=... kids_budget_hit=...
```

### review follow-up

- DS review 指出的 `prune_inode_objects_budgeted` wrap off-by-one 已修：回绕范围为 `..start_ino`，不再包含已扫描的 `start_ino`。
- children prune 的 parent budget 已收紧为 raw `inode_objects` entry 扫描预算；如果 registry 中大量 Weak 已 stale，cursor 会跳过本轮已扫描 entry，避免只限制 live parent 数导致隐藏全表扫描。

## 2. 已完成的编译验证

Codex 已完成：

- Docker 临时副本 `/tmp/mango-build.apdYbA/os`：`make rv64-kernel-build-only` ✅
- Docker 临时副本 `/tmp/mango-build.apdYbA/os`：`make la64-kernel-build-only` ✅
- `git diff --check` ✅

说明：当前容器 `/app` 与宿主主工作区存在漂移，直接在 `/app` 编译不能代表这次 patch。Codex 在容器 `/tmp` 复制 `/app` 基线后覆盖当前源码完成了上述双架构验证。DS 复测时以主工作区实际源码为准。

未跑 QEMU 场景测试；下面由 DS 补实验。

## 3. 实验输入

使用当前主工作区代码，不要切旧 commit。

每个 scenario 前必须恢复 clean image。不要复用上一个 scenario 的污染镜像。

优先使用已有配置：

| 场景 | 配置 |
|------|------|
| S0 | `cc-codex/scenario-s0-lmbench-only.conf` |
| S1 | `cc-codex/scenario-s1-short-pollution.conf` |
| S2b | `cc-codex/scenario-s2b-libcbench-only.conf` |
| S2a | `cc-codex/scenario-s2a-iozone-only.conf` |

## 4. 必跑矩阵

先只跑 rv64，一轮即可：

1. `S0 = lmbench-only`
2. `S1 = basic+busybox+lmbench`
3. `S2b = basic+busybox+libcbench+lmbench`

如果三项都有效，再补：

4. `S2a = basic+busybox+iozone+lmbench`

如果 S1/S2b 指标明显恢复，再跑最终确认：

5. `F5 = full order 到 lmbench`

LA64 不纳入本轮性能判断。LA64 full 之前在 LTP `clone09 + CLONE_NEWNET` 有 kernel stack overflow，属于单独正确性问题。

## 5. 样本有效性要求

每个 raw log 必须满足：

- 无 `panic` / `panicked`
- 有 `[initproc] run_selected_groups done`
- musl/glibc 都有 `[profile] begin lmbench-...`
- musl/glibc 都有 `[profile] end lmbench-...`
- musl/glibc 都有 `[timer] group lmbench ... took ...s`
- musl/glibc profile dump 中都有：
  - `=== ext4 I/O Profile:`
  - `=== reclaim Profile:`
  - `reclaim_budget ...`
  - `reclaim_stage_prune_kids ...`

无效样本不要混入结论，只放到 invalid section。

## 6. 必须解析的字段

### lmbench

至少解析：

- group time
- `Simple syscall`
- `Simple read`
- `Simple write`
- `Simple stat`
- `Simple fstat`
- `Simple open/close`
- `Pipe latency`
- `Pipe bandwidth`
- `Process fork+exit`
- `Process fork+execve`
- `Pagefaults on /var/tmp/XXX`
- `bw_file_rd io_only`
- `bw_file_rd open2close`
- context switch: `2/4/8/16/24/32/64/96`

### reclaim total/stage

至少解析：

- `reclaim calls`
- `reclaim runs`
- `reclaim cycles_total`
- `reclaim cycles_max`
- `io_removed`
- `pc_removed`
- `kids_removed`
- `clean_freed`
- `cached_pages_max`
- `heap_pressure_runs`
- `heap_critical_runs`
- `reclaim_stage_prune_io calls/cycles_total/cycles_max`
- `reclaim_stage_prune_pc calls/cycles_total/cycles_max`
- `reclaim_stage_prune_kids calls/cycles_total/cycles_max`
- `reclaim_stage_cache_metric calls/cycles_total/cycles_max`
- `reclaim_stage_shrink calls/cycles_total/cycles_max`

### 新增 budget counters

必须解析：

- `io_scanned`
- `io_budget_hit`
- `kids_parents_scanned`
- `kids_entries_scanned`
- `kids_budget_hit`

### ext4 counters

继续解析：

- `dir_full_scan_count`
- `dir_full_scan_entries`
- `dir_cache_linear_scan`
- `dir_cache_scanned_entries`
- `dentry_lookup_count`
- `dentry_cache_hit`
- `dentry_cache_miss`
- `children stale_weak`
- `inode_cache hit/miss`

## 7. 对照基线

和以下两组旧数据对比：

### 旧 HEAD / DS 插桩

来源：`cc-codex/results-20260618-ds/`

关键点：

- HEAD S0 musl: open `268.2us`, stat `228.0us`, pipe `504.6us`
- HEAD S1 musl: open `434.2us`, stat `463.8us`, pipe `733.3us`
- HEAD S2 musl: open `448.1us`, stat `240.8us`, pipe `559.4us`

### 16 次 batching fix

来源：`cc-codex/results-20260618-fix/`

关键点：

- S0 musl: open `256us`, stat `217us`, pipe `456us`
- S1 musl: open `1655us`, stat `1300us`, pipe `2244us`
- S1 musl: `prune_kids cycles_max=779,770,056`
- S2b musl: open `1293us`, stat `916us`, pipe `1775us`

## 8. 判定标准

### P0 成功条件

- `S1 prune_kids cycles_max` 不再出现亿级尖刺。
  - 理想：低于 `50M cycles`
  - 硬失败：仍高于 `200M cycles`
- `S1/S2b` 的 `open/stat/pipe/read/write/bw_file_rd` 相比 `results-20260618-fix` 明显恢复。
- `dir_full_scan_count` 继续为 `0`。
- 无 panic。

### P1 成功条件

- `kids_budget_hit` 在污染场景中可以非零，但不应伴随 `cycles_max` 巨大尖刺。
- `kids_entries_scanned / reclaim runs` 接近预算级别，说明清理被分摊。
- `reclaim_stage_prune_kids cycles_total` 可以不低于 batching fix，但 `cycles_max` 必须显著下降。

### 失败判据

任一项出现即标红：

- `prune_kids cycles_max > 200M`
- `open/stat/pipe` 比 `results-20260618-fix` 更差
- `dir_full_scan_count > 0`
- 任何 scenario panic
- `kids_budget_hit` 很高但 `kids_removed` 长期为 0 且用户态指标恶化，说明预算在空扫

## 9. 交付路径

请输出到：

```text
cc-codex/results-20260618-incremental-prune/
```

目录结构：

```text
raw/
  incremental-rv64-S0-round1.log
  incremental-rv64-S1-round1.log
  incremental-rv64-S2b-round1.log
  incremental-rv64-S2a-round1.log   # 若执行
  incremental-rv64-F5-round1.log    # 若执行
parsed/
  lmbench-metrics.json
  reclaim-profiles.json
  ext4-profiles.json
  combined.csv
report.md
manifest.json
```

`manifest.json` 必须包含：

- git commit / dirty diff hash
- 是否包含 Codex incremental prune patch
- 每个 scenario 的 config 文件路径
- clean image 恢复方式
- raw log 文件名
- valid/invalid 样本列表

`report.md` 必须先给结论，再给表：

1. 样本有效性表
2. S0/S1/S2b 对比表
3. 和 `results-20260618-fix` 的 delta 表
4. `prune_kids cycles_max` 专门表
5. `reclaim_budget` 专门表
6. ext4 O(n) 复发检查
7. 是否建议 Codex 继续推进 dirty/event-driven 标记

## 10. 给 Codex 的最小结论格式

最后请用一段话汇总：

```text
Codex：incremental prune 复测完成。S0/S1/S2b 有效/无效情况：...。
关键结果：S1 prune_kids cycles_max 从 779M -> X；S1 open/stat/pipe 从 1655/1300/2244us -> A/B/C；S2b open/stat/pipe 从 1293/916/1775us -> D/E/F。dir_full_scan_count=...。kids_budget_hit=...，kids_entries_scanned=...。结论：建议/不建议继续做 dirty/event-driven。
```
