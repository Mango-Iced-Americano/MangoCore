---
title: "统一内核观测系统 (perf_diag)"
category: debug
status: stable
author: MangoCore Team
last_update: 2026-06-19
tags: [perf, trace, stats, debugging, sysfs, diag]
---

# 统一内核观测系统 (perf_diag)

## 概述

perf_diag 将内核中散落的 trace、perf、stats 三套观测机制统一收敛到 `/sys/kernel/` 文件接口下。统计计数器编译期零开销（feature 关闭时编译为 no-op），运行时通过 `stats_on` AtomicBool 控制，数据通过 `cat` 文件以 `key=value` 文本格式暴露。

## 架构

```
┌────────────────────────────────────────────────────┐
│  编译期: Cargo feature perf_diag                    │
│    ├─ 关闭: 所有 hook 编译为 no-op                  │
│    └─ 开启: hook 编译为 AtomicUsize RMW             │
├────────────────────────────────────────────────────┤
│  运行时: /sys/kernel/stats/stats_on (AtomicBool)    │
│    ├─ 0: hook 内 AtomicBool::load → 立即返回        │
│    └─ 1: 执行计数器更新                             │
├────────────────────────────────────────────────────┤
│  暴露面: sysfs 文件接口                             │
│    /sys/kernel/stats/{taskq,timer,syscall,...}      │
│    /sys/kernel/tracing/{tracing_on,trace,...}       │
└────────────────────────────────────────────────────┘
```

## 构建

```bash
# 竞赛构建（零开销，无 /sys/kernel/ 目录）
make rv64-kernel-build-only

# 诊断构建（带 perf_diag）
make rv64-kernel-build-only EXTRA_FEATURES=perf_diag
make rv64-only EXTRA_FEATURES=perf_diag     # 含用户态
make la64-kernel-build-only EXTRA_FEATURES=perf_diag
```

## 验证 Feature 开启

启动日志:
```
[kernel] perf_diag features: perf_stats=true perf_diag=true
```

运行时:
```bash
cat /sys/kernel/stats/features
# perf_stats=true
# perf_diag=true
# heap_trace=false
```

## 文件参考

### /sys/kernel/stats/

| 文件 | 权限 | 说明 |
|------|------|------|
| `features` | ro | 编译期 feature 状态（perf_stats / perf_diag / heap_trace） |
| `stats_on` | rw | 运行时统计开关（0=关闭 / 1=开启） |
| `reset` | wo | 重置所有 delta 计数器 |
| `taskq` | ro | 调度队列指标（15 项） |
| `timer` | ro | 内核计时器指标（9 项） |
| `syscall` | ro | Syscall/trap 延迟（4 项） |
| `resource` | ro | 资源 gauge（内存/Task/Socket/Pipe/PageCache/Dentry 等） |
| `buddyinfo` | ro | Buddy 空闲块直方图（order → free_blocks） |
| `zombies` | ro | Zombie 按 parent PID 分组 Top10 |

### /sys/kernel/tracing/

| 文件 | 权限 | 说明 |
|------|------|------|
| `tracing_on` | rw | 追踪开关（0=关闭 / 1=开启） |
| `trace` | ro | Ring buffer 文本快照 |
| `dropped` | ro | 丢弃事件计数 |
| `buffer_size` | ro | 环形缓冲容量（固定 2048 entries） |
| `clear` | wo | 清空 ring buffer 并重置 dropped 计数器 |
| `trigger` | wo | 触发一次性资源扫描（接受: `buddy` / `zombie` / `heap`） |

## 使用流程

### 手动诊断

```bash
# 开启统计
echo 1 > /sys/kernel/stats/stats_on
echo 1 > /sys/kernel/stats/reset

# 运行负载
busybox ls -la / > /dev/null

# 查看统计
cat /sys/kernel/stats/taskq
cat /sys/kernel/stats/timer
cat /sys/kernel/stats/syscall
```

### 追踪调试

```bash
# 开启追踪
echo 1 > /sys/kernel/tracing/tracing_on

# 运行负载...

# 查看追踪
cat /sys/kernel/tracing/trace

# 清空并重新开始
echo 1 > /sys/kernel/tracing/clear
```

## 计数器参考

### taskq（调度队列）

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `ready_len_max` | max | 就绪队列历史最大长度 |
| `interruptible_len_max` | max | 可中断队列历史最大长度 |
| `ready_zombie_max` | max | 就绪队列中 zombie 历史最大数 |
| `interruptible_zombie_max` | max | 可中断队列中 zombie 历史最大数 |
| `dup_enqueue_total` | counter | 重复入队次数 |
| `add_ready_total` | counter | 加入就绪队列总次数 |
| `add_interruptible_total` | counter | 加入可中断队列总次数 |
| `wake_interruptible_total` | counter | 唤醒可中断任务总次数 |
| `fair_pick_calls` | counter | O(n) fair 调度次数 |
| `fast_path_calls` | counter | O(1) fast path 调度次数 |
| `fair_scan_max` | max | fair pick 最大扫描深度 |
| `zombie_drain_scan_total` | counter | zombie 清理扫描总次数 |
| `zombie_drain_calls` | counter | zombie drain 调用次数 |
| `zombie_drain_removed` | counter | zombie drain 移除总数 |
| `ready_nonzero_nice_cur` | gauge | 当前 nice≠0 任务数 |

### timer（内核计时器）

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `ktimer_len_max` | max | 计时器队列历史最大长度 |
| `ktimer_add_total` | counter | 添加计时器总次数 |
| `ktimer_pop_max` | max | 单次 pop 最大计时器数 |
| `ktimer_pop_total` | counter | pop_expired 调用次数 |
| `ktimer_stale_waketask` | counter | stale WakeTask 数量 |
| `ktimer_real_wake` | counter | 实际唤醒次数 |
| `ktimer_compact_calls` | counter | compact 调用次数 |
| `ktimer_stale_removed` | counter | compact 移除 stale 数 |
| `wait_with_timeout_total` | counter | wait_with_timeout 调用次数 |

### syscall（系统调用）

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `syscall_total` | counter | 系统调用总次数 |
| `syscall_getppid_total` | counter | getppid（syscall 173）调用次数 |
| `syscall_cost_max_ticks` | max | 单次 syscall 最大耗时（rdcycle） |
| `trap_enter_cost_max_ticks` | max | 单次 trap 最大耗时（rdcycle） |

### resource（资源 gauge）

| 计数器 | 含义 |
|--------|------|
| `ready_tasks` | 当前就绪任务数 |
| `interruptible_tasks` | 当前可中断任务数 |
| `free_frames` | 空闲物理页帧数 |
| `heap_free_kb` | 堆空闲大小（KB） |
| `heap_total_kb` | 堆总大小（KB） |
| `heap_alloc_actual_kb` | 堆实际分配大小（KB） |
| `heap_waste_kb` | 堆浪费大小（KB） |
| `tcp_sockets` | TCP socket 数量 |
| `udp_sockets` | UDP socket 数量 |
| `raw_sockets` | RAW socket 数量 |
| `pending_sockets` | 待处理 socket 数量 |
| `pipe_buf_alive` | 活跃 pipe 缓冲区数 |
| `pipe_buf_bytes_kb` | pipe 缓冲区占用（KB） |
| `unix_ring_alive` | 活跃 Unix ring buffer 数 |
| `unix_ring_bytes_kb` | Unix ring buffer 占用（KB） |
| `mountfs_alive` | 活跃 MountFS 数 |
| `mountfs_inode_alive` | 活跃 MountFSInode 数 |
| `dc_evict_total` | Dentry 淘汰总数 |
| `dc_evict_sole` | Dentry 淘汰（仅 ref）数 |
| `dc_evict_extern` | Dentry 淘汰（外部引用）数 |
| `dc_advance_removed` | Dentry advance 移除数 |
| `pc_registry_len` | PageCache 注册表长度 |
| `pc_registry_alive` | PageCache 注册表活跃项 |
| `pc_registry_stale` | PageCache 注册表 stale 项 |
| `pc_entries_len` | PageCache 条目表长度 |
| `pc_entries_live` | PageCache 条目表活跃项 |
| `pc_entries_holes` | PageCache 条目表空洞数 |

## Initproc 集成

在 `os_test.conf` 中设置 `diag=1`，每组测试每个 libc 完成时自动打印 stats：

```ini
mask=0xFFF
diag=1
```

输出格式:
```
[initproc] [diag] === stats T0 basic:musl ===
ready_len_max=4
...
[initproc] [diag] === stats T0 basic:musl end ===
```

### 工作流程

1. 每组测试开始前，initproc 自动执行 `echo 1 > /sys/kernel/stats/stats_on` 和 `echo 1 > /sys/kernel/stats/reset`
2. 测试运行完毕后，initproc 依次 `cat` 六个 stats 文件（taskq / timer / syscall / resource / buddyinfo / zombies）
3. 分别针对 musl 和 glibc 各输出一次快照

## 竞赛构建

perf_diag feature 关闭时（默认构建）：

- `/sys/kernel/` 目录**不会被创建**（`#[cfg(feature = "perf_diag")]` 守卫）
- 所有 `record_*` hook 编译为 no-op（通过 `#[cfg(not(feature = "perf_stats"))]` + `#[inline(always)]`）
- 热路径零开销：无额外的 load/test/branch 指令

## 故障排查

| 症状 | 原因 | 解决 |
|------|------|------|
| `/sys/kernel/` 不存在 | perf_diag feature 未开启 | 重新构建 `EXTRA_FEATURES=perf_diag` |
| `echo > stats_on` 报 ENOSYS | 内核版本过旧（缺 resize 支持） | 更新到有此功能的 commit |
| 所有计数器恒为 0 | 未带 `EXTRA_FEATURES=perf_diag` 构建 | 检查 `cat /sys/kernel/stats/features` |
| `stats_on` 写入成功但计数器仍 0 | 写入在 open 阶段失败（O_TRUNC 旧 bug） | 同上，检查 feature 状态 |
| `syscall_getppid_total` 为 0 | 内核 syscall ID 173（getppid）未被调用 | 正常，lmbench `lat_syscall null` 使用 getppid |
| trace 无输出 | `tracing_on` 为 0 或被 `clear` 清空 | `echo 1 > /sys/kernel/tracing/tracing_on` |

## 实现文件

| 文件 | 职责 |
|------|------|
| `os/Cargo.toml` | `perf_diag = ["perf_stats"]` feature 定义 |
| `os/src/task/perf.rs` | P0 AtomicUsize 计数器 + record 函数（878 行） |
| `os/src/task/manager.rs` | 调度 + 计时器插桩点 |
| `os/src/task/processor.rs` | 调度循环队列快照 |
| `os/src/syscall/mod.rs` | Syscall 入口/出口计时 |
| `os/src/hal/arch/*/trap/mod.rs` | Trap enter 计时 |
| `os/src/trace.rs` | Ring buffer + tracing_on/dropped 运行时控制 |
| `os/src/fs/sysfs/mod.rs` | sysfs 写支持（write_fn + write_at + resize） |
| `os/src/fs/sysfs/files/diag.rs` | /sys/kernel/ 文件注册与内容格式化 |
| `os/src/fs/sysfs/files/mod.rs` | Feature-gated 注册入口 |
| `user/src/bin/initproc.rs` | diag 模式自动 snapshot |

## 参考

- Linux ftrace / tracefs / debugfs 设计模式
- `.sisyphus/plans/unified-perf-diag.md` — 完整方案文档
- Oracle 评审: bg_acb78f76, bg_6c533974, bg_3a57185e, bg_8ad48260
