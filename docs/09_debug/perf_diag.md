---
title: "统一内核观测系统 (perf_diag)"
category: debug
status: stable
author: MangoCore Team
last_update: 2026-08-08
tags: [perf, trace, stats, debugging, sysfs, diag]
---

# 统一内核观测系统 (perf_diag)

> 2026-07-16 Python 实板性能检查的停止点、18 项 production 基线、三项问题证据与诊断构建结构偏差，见 [2K1000LA Python 性能专项](la64_on_board/260717/README.md)。该批次明确区分 production 正式数字、诊断机制证据、strict-align 第一次实验和未完成项，并保留完整文本原始数据。

## 概述

perf_diag 将内核中散落的 trace、perf、stats 三套观测机制统一收敛到 `/sys/kernel/` 文件接口下。统计计数器编译期零开销（feature 关闭时编译为 no-op），运行时通过 `stats_on` AtomicBool 控制，并用 `profile` 将热点探针限制在单个诊断组；数据通过 `cat` 文件以 `key=value` 文本格式暴露。

## 架构

```
┌────────────────────────────────────────────────────┐
│  编译期: Cargo feature perf_diag                    │
│    ├─ 关闭: 所有 hook 编译为 no-op                  │
│    └─ 开启: hook 编译为 AtomicUsize RMW             │
├────────────────────────────────────────────────────┤
│  运行时: /sys/kernel/stats/{stats_on,profile}       │
│    ├─ stats_on=0: hook 立即返回且不读硬件时钟       │
│    └─ stats_on=1: 只记录 profile 选中的计数器       │
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
| `profile` | rw | `core` / `memory_io` / `network_runtime`；诊断窗口一次只启用一组 |
| `reset` | wo | 重置所有 delta 计数器 |
| `boot` | ro | 从 Rust 入口起算的 console/MM/driver/net/FS/initproc/scheduler 累计 ticks；不随 `reset` 清零 |
| `taskq` | ro | 调度队列、wake/steal 与新任务放置指标（schema v3） |
| `timer` | ro | 内核计时器指标（9 项） |
| `syscall` | ro | Syscall/trap 延迟（4 项） |
| `vm` | ro | filemap、VM 锁/TLB、exec 路径和 MM 切换归因 |
| `blockio` | ro | VirtIO 与 2K1000LA SATA 请求、字节和耗时 |
| `anon_unmap` | ro | private anonymous VMA 释放次数、页数、精确 retain 扫描步数和耗时 |
| `net` | ro | poll、RX/TX/drop 与 exec/openat/read/mmap 运行时归因 |
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
# 先选组，再开启统计
echo core > /sys/kernel/stats/profile
echo 1 > /sys/kernel/stats/reset
echo 1 > /sys/kernel/stats/stats_on

# 运行负载
busybox ls -la / > /dev/null

# 查看统计
cat /sys/kernel/stats/taskq
cat /sys/kernel/stats/timer
cat /sys/kernel/stats/syscall

# 下一个窗口切换到内存/文件/块设备组
echo 0 > /sys/kernel/stats/stats_on
echo memory_io > /sys/kernel/stats/profile
echo 1 > /sys/kernel/stats/reset
echo 1 > /sys/kernel/stats/stats_on
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

正式采样固定使用“关闭追踪 → `stats_on=0` → 选择 `profile` → `reset` → 前快照 → `stats_on=1` → 工作负载 → `stats_on=0` → 后快照”。`all`（数值 7）仅用于接口排障，不用于正式性能结论。

| profile | 数值 | 覆盖范围 |
|---------|------|----------|
| `core` | 1 | 启动后调度、timer、futex、syscall/trap |
| `memory_io` | 2 | 缺页、TLB、frame/heap、PageCache、VFS、VirtIO/SATA |
| `network_runtime` | 4 | poll、RX/TX/drop，以及 Python 相关 exec/openat/read/mmap |

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
| `new_task_idle_available` | counter | 新任务发布时允许集合中存在空闲 CPU 的次数 |
| `new_task_selected_idle` | counter | 新任务发布实际选择空闲 CPU 的次数 |
| `new_task_kept_busy_parent` | counter | 所有允许 CPU 都忙时仍保留创建者 CPU 的次数 |

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
| `syscall_cost_max_ticks` | max | 单次 syscall 最大耗时（timer timebase tick） |
| `trap_enter_cost_max_ticks` | max | 单次 trap 最大耗时（timer timebase tick） |
| `user_unaligned_traps` | counter | 用户态非对齐访存异常总数（LoongArch） |
| `user_unaligned_ticks_total/max` | counter/max | 非对齐 Rust handler 的累计/最大耗时；不含汇编 trap entry/restore |
| `user_unaligned_load_{2,4,8}` | counter | 按访问宽度分类的非对齐 load |
| `user_unaligned_store_{2,4,8}` | counter | 按访问宽度分类的非对齐 store |
| `user_unaligned_float_{loads,stores}` | counter | 解码为浮点访存的非对齐异常 |

非对齐计数必须包住 workload body，不能把解释器启动/import 混入。对 store 型负载，建议同时采集 `memory_io` 的 `tlb_page`：`sum(store_width * store_count)` 与 `tlb_page` 接近时，说明逐字节模拟正在放大 private-store/COW/TLB 路径。handler ticks 只覆盖 Rust 分支，完整异常成本还包括 GP/FPR/LSX 保存恢复，因此只能作为下界。

### memory_io（内存、PageCache、块设备）

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `page_faults` / `pagefault_ticks_total` | counter | 缺页次数与 `do_page_fault()` 累计 timer timebase ticks；完整 lmbench wall time 还包含该边界外的 trap 和用户态部分 |
| `frame_alloc_hits` / `frame_alloc_ticks_total` | counter | frame 分配次数与累计 timer timebase ticks |
| `frame_free_hits` | counter | frame 释放次数 |
| `tlb_{full,page,activate,global}` | counter | 各类 TLB 操作；`activate` 不是实际地址空间切换数 |
| `pc_read/write/wb_*` | counter | PageCache 读、写、写回次数、页数和 ticks |
| `sata_read/write_{reqs,bytes,ticks_total}` | counter | 2K1000LA AHCI 数据请求、字节与累计完成耗时 |
| `sata_flush_{reqs,ticks_total}` | counter | SATA cache flush 次数与累计耗时 |
| `journal_commit_{count,bytes}` | counter | 成功完成的 another_ext4 journal transaction 数及其 journal payload 字节数（descriptor/data/revoke + commit block） |
| `device_flush_count` | counter | 实际提交到 VirtIO 块设备的 flush 请求数 |
| `virtio_write_{requests,bytes}` | counter | MMIO/PCI VirtIO 在 DMA fallback 分片后实际提交的写请求数及字节数 |
| `virtio_read_requests` | counter | MMIO/PCI VirtIO 在 DMA fallback 分片后实际提交的读请求数 |
| `virtio_dma_small_pool_enabled` | gauge | 固定小描述符 DMA 池是否成功初始化 |
| `virtio_dma_share_{header,status,indirect}_pool` | counter | block VirtIO 请求头、状态字节、间接描述符从小池复用的次数 |
| `writeback_{batch_count,page_count}` | counter | 成功完成的 PageCache writeback run 数与页数 |
| `pc_write_{lookup,lease,copy,commit}_cycles` | counter | `PageCache::write_user` 中 PageEntries 查找、写 lease、用户缓冲复制及 Dirty 发布的累计周期；仅在 `memory_io` profile 下记录 |
| `ext4_pc_readpages_{calls,pages}` | counter | PageCache 后端批量读取的调用数与页数；another_ext4 demand miss 每次最多 16 页（64 KiB） |
| `ext4_pc_readpages_runs` | counter | legacy ext4 后端按物理连续块合并的读取 run 数（another_ext4 不使用该字段） |
| `wb_tx_data_write_{calls,bytes,ticks}` | counter | another_ext4 journal-backed data write 的次数、字节数与累计 ticks |
| `wb_tx_alloc_extent_{calls,pages,ticks}` | counter | data write 路径中 alloc/extent 准备的次数、页数与累计 ticks |
| `wb_tx_journal_{commit_ticks,staged_blocks,tx_first,tx_last}` | counter/gauge | 已提交 journal transaction 的累计 ticks、staged block 数及本窗口 transaction id 范围 |
| `wb_tx_journal_flush_{count,ticks}` | counter | `ActiveLog`、`CommitRecord`、`Checkpoint`、`TailUpdate` 四个 journal phase 的设备 flush 次数与累计 ticks |
| `wb_tx_boundary_flush_{count,ticks}` | counter | journal 外明确 durability boundary 的设备 flush 次数与累计 ticks |

`clock_freq_hz` 是上述 perf timer tick 的唯一换算分母：`µs = ticks × 1_000_000 / clock_freq_hz`。不要将它与 RV64 `rdcycle` 或跨架构 CPU cycle 数混用。

`/sys/kernel/stats/pagefault` 同时导出互斥的 `action_*` 分类（例如 `FileBackedRead`）和嵌套的 `stage_*` 计时（例如 `pte_map`、`tlb_flush`、`filemap_frame`）。action 可按 count 合计；stage 可能相互包含，不能相加后当作总 handler 时间。`trap_entry` 与 `trap_return` 当前只覆盖 Rust 侧边界，不覆盖 trampoline 汇编保存、恢复或 `sret`。

another_ext4 的 transaction 诊断会在串口输出 `[wb_txn]` 事件：`commit` 带 transaction id、reason 和 staged blocks，`flush` 带四个 journal phase，`boundary_flush` 单独标记 `DurabilityBoundary`。因此分析时必须先按 reason 与 phase 分类：一个 `commit_journal` 当前固定会产生四个 phase flush，且 durability-boundary flush 不是额外 journal commit。`staged_blocks=0` 仅说明该 deferred journal 的 staging 数量，不能推断直接 metadata 操作没有 I/O。

#### Stage 0：VM/filemap/exec 归因

阶段 0 的第一批计数器用于回答“时间是在地址空间锁、文件映射后端，还是 exec 路径选择上消耗”的问题。filemap 和 VM
锁/flush 计数器属于 `memory_io` profile，exec 与 MM 切换计数器属于 `core` profile；只有对应 profile 被选中且
`stats_on=1` 时才执行原子更新和取时钟，默认竞赛构建仍为编译期 no-op。`filemap_ready_hit` 表示
`frame_for_filemap_read()` 返回可用 frame，不等同于磁盘缓存命中；后端读计时覆盖 PageCache 的 filemap 读取调用窗口，
`under_vm` 用于区分该调用是否发生在 AddressSpace 写锁保护范围内。

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `filemap_{read,private,shared_write}_fault_calls` | counter | 三类 filemap fault 入口次数 |
| `filemap_ready_hit` | counter | read fault 获得可用 frame 的次数 |
| `filemap_not_ready_retry` | counter | PageCache 返回 Retry、需要稍后重试的次数 |
| `filemap_backend_read_calls` | counter | filemap 调用 PageCache 后端读取的次数 |
| `filemap_backend_read_ticks_total` | counter | 上述 PageCache 读取累计 ticks |
| `filemap_backend_read_under_vm_calls` | counter | 后端读取发生在 VM 写锁内的次数 |
| `filemap_fault_around_calls/pages_requested` | counter | filemap 冷缺页触发的受限窗口次数与请求页数 |
| `filemap_fault_around_pages_missing/published` | counter | admission 时实际缺页数与代际重验后实际发布页数 |
| `filemap_fault_around_pages_prefetched` | counter | 发布页中除 demand 页外带 readahead 标记的页数 |
| `filemap_fault_around_backend_runs` | counter | 连续 miss run 产生的真实后端 `read_pages` 调用数 |
| `filemap_fault_around_useful_hits` | counter | readahead 页随后被 PageCache/filemap 消费的次数 |
| `filemap_fault_around_unused_discards` | counter | readahead 页未使用即被回收、truncate 或 invalidate 的次数 |
| `filemap_fault_around_aborts` | counter | 批量 admission 因 I/O、内存或 generation 变化放弃的次数 |
| `exec_direct_count` | counter | exec 尝试 direct ELF loader 的次数 |
| `exec_direct_enosys_count` | counter | direct loader 返回 ENOSYS 的次数 |
| `exec_fallback_count` | counter | 回退到通用 ELF loader 的次数 |
| `vm_{read,write}_lock_calls` | counter | AddressSpace 读/写锁成功获取次数 |
| `vm_{read,write}_lock_wait_ticks_total` | counter | 获取锁前等待累计 ticks |
| `vm_{read,write}_lock_hold_ticks_total` | counter | 持锁执行操作累计 ticks |
| `vm_flush_outside_lock_ticks_total` | counter | 释放 VM 写锁后执行 TLB flush 的累计 ticks |
| `task_switch_{same,different}_mm` | counter | 调度切换中复用/切换地址空间的次数（core profile） |

推荐采样窗口：

```sh
echo 0 > /sys/kernel/stats/stats_on
echo memory_io > /sys/kernel/stats/profile
echo 1 > /sys/kernel/stats/reset
echo 1 > /sys/kernel/stats/stats_on
# 运行一个明确边界的 mmap/exec/filemap workload
echo 0 > /sys/kernel/stats/stats_on
cat /sys/kernel/stats/vm
```

如果同时需要 `task_switch_same_mm/different_mm` 或 exec 路径计数，应在另一个窗口选择 `core`（或仅在接口排障时使用
`all`），避免把两个 profile 的原子开销混入同一次正式基线。

#### anonymous private VMA release

`anon_unmap` 只在 `memory_io` profile 下记录 anonymous + private VMA；file/shared mapping
不进入该组。计时覆盖 `Vma::unmap` 内部，`retain_scan_steps_total` 在每次现有
`VecDeque::retain` 之前累加当时 `active.len()`，因此是实际扫描量而不是按页数推算。

| 计数器 | 类型 | 含义 |
|--------|------|------|
| `anon_unmap_calls_total` | counter | 满足记录条件的 VMA unmap 调用数 |
| `anon_unmap_{range,area}_calls` | counter | range unmap 与 remove-area 来源分类 |
| `anon_unmap_requested_pages_total` | counter | 调用请求范围页数，含未 resident 页 |
| `anon_unmap_resident_pages_total` | counter | 实际删除的 resident 页数 |
| `anon_unmap_active_before_total/max` | counter/max | 调用开始时 frame store active 规模 |
| `anon_unmap_retain_scan_steps_total` | counter | 当前实现所有 retain 的实际遍历元素数 |
| `anon_unmap_ticks_total/max` | counter/max | unmap 累计与最大 rdtime ticks |
| `anon_unmap_errors_total` | counter | 释放过程错误数 |
| `anon_unmap_pages_le_16/le_256/le_4096/gt_4096` | counter | resident pages 分桶 |

合成居民映射的正确性不变量为：单个 N 页 VMA 全部逐页删除时，主项扫描数应为
`N(N+1)/2`。真实 workload 归因必须在目标进程内完成“预热 → reset/on → body → off”，
否则 shell/启动/退出会引入额外 VMA。2026-07-17 的实板量化见
[strict runtime 与匿名释放量化](la64_on_board/260717/07-strict-runtime-and-anon-unmap-quantification.md)。

块设备 ticks 统计的是驱动同步调用窗口，不等于 workload 的完整 I/O wait。用 `(read + write + flush) ticks / sys time` 估算设备直接占比后，剩余时间仍可能位于 VFS/ext4、PageCache、锁、分配和用户复制。若 write 与 flush 近似一一对应，应回到块设备调用点确认 flush 粒度，不能把全部 sys 直接归因于磁盘介质。

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

### 阶段 0 扩展字段

阶段 0 的采样 hook 已扩展到以下低扰动归因域：

- VM 锁等待/持锁最大值、MM activate/deactivate、generation 追赶和 ASID rollover；
- filemap retry wait、后端读最大值及 VM 锁内读耗时；
- 帧分配器全局锁、reserve/OOM、fresh/recycled 来源和 contiguous 页初始化；
- 堆锁等待/持锁、slab class 与 buddy 分配路径；
- 唤醒本地/远程、保持最近 CPU、空闲 CPU 选择、wake-to-run 和运行片段；
- work stealing 尝试、候选、成功、复核失败及 kernel-TLB 同步；
- ELF PT_LOAD 段/页/文件字节、prefetch、目标页分配/清零、PageCache copy 和 fallback kmap 等阶段。

这些字段只通过 `/sys/kernel/stats/taskq` 与 `/sys/kernel/stats/vm` 读取，仍受 `stats_on` 和 profile
门禁控制；默认构建下对应 hook 为 no-op，不向串口输出逐事件日志。

在 `os_test.conf` 中设置 `diag=1`，每组测试每个 libc 完成时自动打印 stats：

```ini
mask=0xFFF
diag=1
```

输出格式:
```
[initproc] [diag] === stats iozone-musl ===
journal_commit_count=126
device_flush_count=830
virtio_write_requests=80093
virtio_write_bytes=400502784
writeback_page_count=18361
...
[initproc] [diag] === stats iozone-musl end ===
```

### 工作流程

1. 每组测试开始前，initproc 自动执行 `stats_on=0`、选择 `memory_io`、`reset` 和 `stats_on=1`
2. 测试运行完毕后，initproc 先关闭统计再读取 `blockio`；手工性能窗口还应保存前后快照，而不是只保存结束绝对值
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
| QEMU 没有非对齐异常、实板大量出现 | 两平台 UAL 能力不同 | 读取实板 CPUCFG，并以实板计数作为最终标准 |
| 计数器能解释 sys、却小于完整 sys | handler ticks 不含汇编入口/出口，或 rusage 未聚合线程/child | 将结果视为下界，并核对 rusage 语义 |

## 实现文件

| 文件 | 职责 |
|------|------|
| `os/Cargo.toml` | `perf_diag = ["perf_stats"]` feature 定义 |
| `os/src/task/perf.rs` | profile-aware AtomicUsize 计数器、时钟门禁与 record 函数 |
| `os/src/task/manager.rs` | 调度 + 计时器插桩点 |
| `os/src/task/processor.rs` | 调度循环队列快照 |
| `os/src/mm/address_space.rs` | AddressSpace 锁等待/持锁与锁外 TLB flush 计时 |
| `os/src/mm/filemap.rs` | filemap fault 类型、PageCache 重试和后端读归因 |
| `os/src/syscall/mod.rs` | Syscall 入口/出口计时 |
| `os/src/syscall/process/exec.rs` | direct ELF loader 与 fallback 路径计数 |
| `os/src/hal/arch/*/trap/mod.rs` | Trap enter 计时 |
| `os/src/trace.rs` | Ring buffer + tracing_on/dropped 运行时控制 |
| `os/src/fs/sysfs/mod.rs` | sysfs 写支持（write_fn + write_at + resize） |
| `os/src/fs/sysfs/files/diag.rs` | /sys/kernel/ 文件注册与内容格式化 |
| `os/src/fs/sysfs/files/mod.rs` | Feature-gated 注册入口 |
| `user/src/bin/initproc.rs` | diag 模式自动 snapshot |
| `scripts/kernel_perf.py` | 源码/镜像指纹、串口 ACK、前后快照、脱敏、JSONL/CSV 分析 |
| `user/tools/cpython/bench/bench_runner.py` | workload body 边界、target-side JSONL 与 CPython benchmark 事件 |

## 2K1000LA Python/ext4 已验证实例

2026-07-16 的 production 正式矩阵和 `perf_diag` 定向窗口保存在：

- `target/perf-runs/20260716T102350Z-cpython-ext4-production/`
- `target/perf-runs/20260716T-cpython-deepdiag/`

该实例证明了三类不同瓶颈必须分开观测：非对齐异常可解释 `bm_string/bm_float` 的大部分 sys；匿名 resident mapping 的关闭时间随 page 数平方增长；ext4 小文件样本中 SATA 直接耗时只占 sys 的一部分。正式结论见 production 目录的 `reports/cpython_ext4_kernel_analysis.md`。`target/` 为未跟踪结果目录，归档或跨机器复现时必须连同 manifest、records.jsonl 和 raw 日志一起保存。

## 参考

- Linux ftrace / tracefs / debugfs 设计模式
- `.sisyphus/plans/unified-perf-diag.md` — 完整方案文档
- Oracle 评审: bg_acb78f76, bg_6c533974, bg_3a57185e, bg_8ad48260
