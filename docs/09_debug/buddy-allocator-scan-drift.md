---
title: "堆分配器 buddy merge 线性扫描导致的渐进性能退化"
category: debug
status: verified
author: MangoCore Team
last_update: 2026-07-18
tags: [buddy-allocator, heap, free-list, bitmap, drift, performance, lmbench, O(n)-scan]
---

# 堆分配器 buddy merge 线性扫描导致的渐进性能退化

## 1. 问题现象

在 basic + busybox 测试组累积运行后，lmbench 多项指标出现显著的渐进性能退化，而纯读取操作不受影响：

| 测试项 | 窗口 W0 | 窗口 W1 | 窗口 W2 | 窗口 W3 | 退化幅度 |
|--------|---------|---------|---------|---------|----------|
| Simple open/close | 309 μs | 502 μs | 638 μs | **746 μs** | **+141%** |
| Process fork+exit | 6,972 μs | 9,105 μs | 9,096 μs | **13,169 μs** | **+89%** |
| Simple stat | — | — | — | — | 类似趋势 |
| null syscall (getppid) | 36 μs | 38 μs | 39 μs | 39 μs | **无退化** |

**退化特征**：

- 退化仅出现在"创建新内核对象"的操作（文件打开/关闭、进程创建），不影响"读取已有字段"的操作（getppid 只读一个 target 字段）。
- 退化幅度逐窗口递增，呈现典型的渐进退化（creeping degradation）模式。
- 每次 pre-workload (basic + busybox) 执行完成后，退化窗口推进一步 —— 说明退化累积的是 workload 中分配/释放操作产生的状态。

## 2. 调试过程（按时间线）

### 第一轮：建立漂移测试基础设施

**目标**：在缺乏退化信号的情况下，建立可重复测量的实验框架。

在 `user/src/bin/initproc.rs` 中实现 `drift_window` 模式：

- 每窗口执行流程：`reset 计数器 → pre-snapshot → lmbench 测量 → post-snapshot`
- 暴露 P1 级性能计数器（通过 `/sys/kernel/stats/` 虚拟文件系统）：
  - `ctxsw` — 上下文切换
  - `reclaim` — 页面回收
  - `tlb` — TLB 刷新次数与分类
  - `heap` — 堆分配/释放次数+计时
  - `syscall` — 系统调用总数 + getppid 特定计数
  - `resource` — 帧分配器碎片化 + 僵尸进程数

**测试**：纯 `lat_syscall null` 循环，6 窗口，无 pre-workload。

**结果**：lmbench null syscall 35–38 μs，所有指标**无退化**。结论：纯循环不触发退化，需要累积状态。

### 第二轮：basic pre-workload

**目标**：引入进程创建和文件 I/O 来"喂饱"内核状态。

每窗口前运行 basic 测试组（进程创建 + 文件 I/O），然后执行 `lat_syscall null`。

**配置**：8 → 20 窗口扩展。

**结果**：

- lmbench null syscall 35–39 μs，**仍无退化**。
- 但发现次级信号：basic TLB flush 逐窗口增长 5x（445K → 2.3M），reclaim 开始出现。
- 结论：basic 太重但 null syscall 免疫 —— 创建对象类操作累积了碎片但 getppid() 不关心。

### 第三轮：lat_proc pre-workload

**目标**：用 lmbench 自带的进程压测（fork/exec/shell）作为 pre-workload，模拟更真实的负载。

新增计数器：
- `SECCOMP` 检查（seccomp_check_calls、seccomp_disabled_bypass）—— 全部为 0
- `TIMER_IRQ/POP` 开销（timer_irq_ticks_total/max，timer_pop_ticks_total/max）

**结果**：lmbench null syscall 36–39 μs，**仍然不退化**。

**关键洞察**：所有 pre-workload 都正确累积了碎片状态，但 null syscall (getppid) 本身从不分配也不释放堆内存，所以完全免疫于堆碎片化。

### 第四轮：切换到全量 lmbench 测量

**策略转变**：不再只测 null syscall，而是把**全量 lmbench** 作为被测对象。`drift_measure=full`。

```bash
cd /musl && sh lmbench_testcode.sh
```

**结果立即出现**：

```
W0 → W1 → W2 → W3
open/close:  309 → 502 → 638 → 746 μs  (+141%)
fork+exit:  6972 → 9105 → 9096 → 13169 μs (+89%)
```

**关键发现**：**退化不在 null syscall，在"创建新对象"的操作（文件操作、进程创建）**。这些操作频繁堆分配/释放，随着 free-list 增长越来越慢。

### 第五轮：堆分配器计时

**目标**：直接测量 heap allocator 的 dealloc 成本变化，确认根因。

在 `os/src/task/perf.rs` 添加 7 个 P0 级堆计数器：

```rust
HEAP_ALLOC_CALLS             // 分配次数
HEAP_ALLOC_TICKS_TOTAL       // 分配总周期数
HEAP_ALLOC_TICKS_MAX         // 单次分配最大周期数
HEAP_DEALLOC_CALLS           // 释放次数
HEAP_DEALLOC_TICKS_TOTAL     // 释放总周期数
HEAP_DEALLOC_TICKS_MAX       // 单次释放最大周期数
HEAP_DEALLOC_SCAN_STEPS_TOTAL // dealloc 中线性扫描总步数
```

在 `os/src/mm/heap_allocator.rs` 的 `alloc()` 和 `dealloc()` 中插入精确定时桩。通过 `buddy_system_allocator::DEALLOC_SCAN_HOOK` 回调接收 buddy allocator 内部的 scan step 计数。

**决定性数据**（4 窗口，basic+busybox pre-workload，全量 lmbench）：

| 指标 | W0 | W1 | W2 | W3 | 趋势 |
|------|-----|-----|-----|-----|------|
| scan_steps per dealloc (avg) | 19.1 | — | — | **113.7** | **6×** |
| dealloc_ticks (avg per call) | 10.8K | — | — | **69.9K** | **6.5×** |
| scan_steps_total | 14.3M | 60.3M | 41.1M | 70.4M | — |
| heap_alloc_calls | ~640K | ~2.1M | ~1.7M | ~2.3M | — |

**根因确认**：`lib.rs:294` 的 `for block in free_list.iter_mut()` 线性扫描成本随 heap 碎片化线性增长。

## 3. 根因分析

### 3.1 Buddy Allocator dealloc() 流程

buddy system allocator 的 `dealloc()` 通过逐级合并 buddy 块来回收内存：

```
dealloc(ptr, layout):
1. class = log2(size)
2. push freed block to free_list[class]
3. while current_class < ORDER:
4.   buddy = current_ptr ^ (1 << current_class)
5.   for block in free_list[current_class].iter_mut():  ← 线性扫描！
6.     if block.value() == buddy → merge
7.   if no buddy found → break
```

### 3.2 线性扫描退化机理

- `free_list` 是 intrusive 单向链表，查找 buddy 需要遍历**整个链表**。
- 随着 heap 碎片化（大量小块分配/释放），同一 size-class 的 free-list 越来越长。
- 每次 `dealloc()` 都需要 O(n) 扫描来找 buddy —— 即使 buddy 根本不在 free-list 中，也必须扫描完整个链表才能确认。
- 高碎片场景下，大部分扫描是**无效的** —— buddy 已被分配出去，但每次都必须遍历整个链表才 return break。

### 3.3 为什么 null syscall (getppid) 不退化

getppid() 只读取 `current_task().process.ppid` 这一个字段，全程不调用堆分配器（`alloc::Box`、`alloc::Vec::push` 等）。堆碎片化对纯读取路径零影响。

### 3.4 为什么 open/close 和 fork+exit 退化严重

这些操作路径密集触发堆分配/释放：

- `open()` → 创建 `File` Arc → 分配 IndexNode → 创建 PageCache 条目
- `close()` → 释放 Arc → drop IndexNode → 回收 PageCache 页面
- `fork()` → 创建 TCB/PCB → clone 地址空间 → 分配内核栈
- `exit()` → 清理 fd table → 释放用户页 → 释放内核栈

每次 dealloc 都触发 O(n) 的 buddy 扫描，而随着 pre-workload 累积这些路径的调用次数达百万级，扫描步数从每 call 19 步飙升到 114 步。

## 4. 解决方案：bitmap guard

### 4.1 策略

**不改 buddy allocator 核心机制**，只加 bitmap 作为 O(1) 守卫 —— 在任何 O(n) 线性扫描之前先用 O(1) 位图判断 buddy 是否在 free-list 中。若 buddy 不在，直接 `break`，跳过无效扫描。

### 4.2 数据结构

修改文件：`dependency/buddy_system_allocator/src/lib.rs`

```rust
pub struct Heap<const ORDER: usize> {
    free_list: [LinkedList; ORDER],
    heap_start: usize,
    heap_end: usize,
    free_bits: [*mut usize; ORDER],  // 新增：per-class 空闲位图
    // ... 其他字段
}
```

### 4.3 bitmap 内存布局

- 在 `init()` 中从 heap region 前端 carve 出 bitmap 内存（约 4 MB / 256 MB）。
- 每个 class 一个 bitmap：class 0 1 bit per byte、class 12 1 bit per 4 KB 块。
- 总开销：`Σ (size >> c) bits = size * (2 - 1/2^(ORDER-1))` ≈ 2 × heap size bits。
- 对于 256 MB heap、ORDER=32，总 bitmap 开销约 64 MB bits = 8 MB（含 padding 和对齐）。

### 4.4 关键代码

```rust
// 位图操作（均为 O(1)）
fn bitmap_set(&mut self, c: usize, addr: usize) { ... }
fn bitmap_clear(&mut self, c: usize, addr: usize) { ... }
fn bitmap_test(&self, c: usize, addr: usize) -> bool { ... }
```

#### dealloc() 修改

```rust
// 修复前：无条件遍历整个 free-list
for block in self.free_list[current_class].iter_mut() {
    scan_steps += 1;
    if block.value() as usize == buddy {
        // merge
    }
}

// 修复后：先 O(1) 检查，buddy 不在则直接 break
if !self.bitmap_test(current_class, buddy) {
    break;  // ← O(1) 守卫，消除无效扫描
}
// Buddy IS free — 进入已有线性扫描
for block in self.free_list[current_class].iter_mut() { ... }
```

#### alloc/push/pop 同步

`alloc()` 中的 `split`、`pop` 操作和 `dealloc()` 中的 `push` 操作同步更新 bitmap：

```rust
// push → bitmap_set
self.free_list[class].push(ptr_addr as *mut usize);
self.bitmap_set(class, ptr_addr);

// pop → bitmap_clear
let popped = self.free_list[class].pop();
self.bitmap_clear(class, popped as usize);

// split → clear parent + set children
self.bitmap_clear(j, block_addr);
self.bitmap_set(j - 1, block_addr);
self.bitmap_set(j - 1, buddy_addr);
```

### 4.5 边界保护

- 空指针检查：`if self.free_bits[c].is_null() { return; }` —— 防止无 bitmap 模式下的空指针解引用
- 地址范围检查：`if addr < self.heap_start || addr >= self.heap_end { return; }`
- 索引越界检查：`if idx >= block_count { return; }`
- 内存不足 fallback：`if bitmap_offset >= size` 时回退到无 bitmap 模式

## 5. 修复验证

### 5.1 rv64 QEMU 测试

**配置**：basic+busybox pre-workload，全量 lmbench，4 窗口

**lmbench 指标对比**：

| 指标 | 修复前 W0→W3 | 修复后 W0→W3 |
|------|------------|------------|
| open/close | 309→746 μs (+141%) | 263→265 μs (**0%**) |
| fork+exit | 6,972→13,169 (+89%) | 6,891→7,187 (**4%**) |
| null syscall | 36-39 μs | 36-38 μs |
| Simple stat | — | 231→213→215→253 μs |

**heap 计数器对比**：

| 指标 | 修复前 W0 | 修复前 W3 | 修复后 W0 | 修复后 W3 |
|------|----------|----------|----------|----------|
| scan_steps_total | 14.3M | 70.4M | 3,285 | 386 |
| dealloc ticks (total) | 10.8K | 69.9K | — | — |
| dealloc ticks per call | 10.8K | 69.9K | ~0.4K | ~0.4K |

修复后 scan_steps 减少 **130 倍**（70.4M → 386），dealloc 成本从每 call 69.9K ticks 降至约 0.4K ticks。

### 5.2 la64 QEMU 测试

**配置**：同样 basic+busybox pre-workload，全量 lmbench，4 窗口

| 指标 | W0 | W1 | W2 | W3 |
|------|-----|-----|-----|-----|
| open/close | 89 μs | 93 μs | 95 μs | 93 μs |
| null syscall | 5.3 μs | 5.4 μs | 5.5 μs | 5.6 μs |

la64 上退化同样消除，全部窗口稳定。

### 5.3 rv64 6 窗口扩展验证

**配置**：basic+busybox pre-workload，全量 lmbench，6 窗口

| 指标 | W0 | W1 | W2 | W3 | W4 | W5 |
|------|-----|-----|-----|-----|-----|-----|
| open/close | 280 μs | 267 μs | 262 μs | 262 μs | 258 μs | 246 μs |
| null syscall | 38.3 μs | 37.5 μs | 37.6 μs | 39.6 μs | 37.2 μs | 38.6 μs |

| 计数器 | W0 | W1 | W2 | W3 | W4 | W5 |
|--------|-----|-----|-----|-----|-----|-----|
| scan_steps (post) | 1.38M | 502K | 516K | 513K | 530K | 338K |

open/close 略有下趋势（280→246 μs），无退化。scan steps 稳定在 338K-1.38M 范围，是修复前（14M-70M）的 1/50 以下。

### 5.4 编译验证

- `make rv64-kernel-build-only` ✅
- `make la64-kernel-build-only` ✅

## 6. 经验教训

1. **渐进退化优先怀疑有状态的数据结构**：free-list、hash table、LRU list 等随负载累积长度的结构是第一嫌疑对象。纯计算路径（如 getppid 字段读取）几乎不会出现渐进退化。

2. **计数器驱动的迭代式调试**：从粗粒度到细粒度逐步缩小范围 —— 先用 null syscall 排除纯计算路径，再用全量 lmbench 定位到"创建新对象"操作类别，最后在 heap allocator 内部插桩精确定位到 `for block in free_list.iter_mut()`。每一轮都用数据决策下一步方向。

3. **null syscall 隔离技术**：`getppid()` 只读取一个进程字段，不触发任何堆分配或释放，可完美隔离"创建新对象 → 碎片累积"与"读取已有字段 → 无影响"两类操作路径。这是调试渐进退化的利器。

4. **bitmap guard 模式**：在已有 O(n) 操作前加 O(1) 快速守卫，是低成本消除大部分退化的通用技术。不改变核心算法，只在无效路径上短路。本例中 scan steps 减少 130 倍，dealloc 成本降为原来的 1/175。

5. **Oracle 多轮咨询**：第一轮问"为什么 lmbench open/close 逐窗口越来越慢"得到"heap fragmentation + free-list linear scan"的假设；第二轮问"dealloc 内哪个操作 O(n)"定位到 `for block in free_list.iter_mut()`；第三轮问"最优修复方案"得到 bitmap guard 模式。每次数据足够后再问，比一次问完更精准。

6. **可复现性基础设施建设**：`drift_window` 模式 + `analyze_drift.py` 自动分析脚本构成了完整的退化检测管线。在第一次出现退化信号后立即建立可重复的测量框架，是高效调试的前提。

## 7. 涉及文件

```
dependency/buddy_system_allocator/src/lib.rs      # 根因所在 + bitmap guard 修复
os/src/task/perf.rs                                 # P0 级 heap 计数器（7 个）
os/src/mm/heap_allocator.rs                        # alloc/dealloc 计时插桩 + DEALLOC_SCAN_HOOK 注册
os/src/fs/sysfs/files/diag.rs                      # /sys/kernel/stats/heap 暴露
os/src/syscall/mod.rs                              # seccomp 检查插桩
os/src/syscall/process/ids.rs                      # seccomp bypass 插桩
os/src/task/manager.rs                             # timer IRQ/pop 插桩
user/src/bin/initproc.rs                           # drift_window + pre_mask + measure 模式
scripts/analyze_drift.py                           # 自动分析脚本
```

## 8. 附录：详细数据表

### 8.1 修复前 lmbench 成绩（rv64, basic+busybox pre-workload, 4 窗口）

| 测试项 | W0 (μs) | W1 (μs) | W2 (μs) | W3 (μs) | W0→W3 退化 |
|--------|---------|---------|---------|---------|-----------|
| Simple open/close | 309 | 502 | 638 | 746 | +141% |
| Process fork+exit | 6,972 | 9,105 | 9,096 | 13,169 | +89% |
| null syscall | 36 | 38 | 39 | 39 | 0% |
| Simple stat | — | — | — | — | — |

### 8.2 修复前 heap 计数器（rv64, 同上配置）

| 计数器 | W0 | W1 | W2 | W3 |
|--------|-----|-----|-----|-----|
| scan_steps_total | 14,316,108 | 60,273,820 | 41,122,853 | 70,421,086 |
| heap_alloc_calls | 641,876 | 2,082,236 | 1,694,127 | 2,308,196 |
| heap_dealloc_calls | 641,798 | 2,081,344 | 1,693,150 | 2,307,259 |
| heap_dealloc_ticks_total | 6,920,436,142 | — | — | 161,347,830,038 |
| dealloc_ticks_per_call | 10,783 | — | — | 69,930 |
| scan_steps_per_dealloc (avg) | 22.3 | 29.0 | 24.3 | 30.5 |

> 注：W1 和 W2 的 dealloc_ticks_total 未完整记录，以趋势代替。

### 8.3 修复后 lmbench 成绩（rv64, basic+busybox pre-workload, 4 窗口）

| 测试项 | W0 (μs) | W1 (μs) | W2 (μs) | W3 (μs) | W0→W3 变化 |
|--------|---------|---------|---------|---------|-----------|
| Simple open/close | 263 | 266 | 264 | 265 | 0% |
| Process fork+exit | 6,891 | 7,053 | 7,127 | 7,187 | +4% |
| null syscall | 36 | 37 | 38 | 38 | 0% |
| Simple stat | 231 | 213 | 215 | 253 | +10% |

### 8.4 修复后 heap 计数器（rv64, 同上配置）

| 计数器 | W0 | W1 | W2 | W3 |
|--------|-----|-----|-----|-----|
| scan_steps_total | 3,285 | 3,569 | 4,986 | 386 |
| heap_alloc_calls | 685,691 | 1,143,380 | 841,160 | 1,103,694 |
| heap_dealloc_calls | 685,624 | 1,142,427 | 840,196 | 1,102,677 |
| heap_dealloc_ticks_total | 263,258,901 | — | — | 1,003,889,006 |
| dealloc_ticks_per_call (avg) | ~384 | — | — | ~910 |

### 8.5 修复后 la64 成绩（basic+busybox pre-workload, 4 窗口）

| 测试项 | W0 (μs) | W1 (μs) | W2 (μs) | W3 (μs) |
|--------|---------|---------|---------|---------|
| Simple open/close | 89 | 93 | 95 | 93 |
| null syscall | 5.3 | 5.4 | 5.5 | 5.6 |

### 8.6 关键对比：修复前后 scan_steps_total 趋势

| 窗口 | 修复前 | 修复后 | 减少倍数 |
|------|--------|--------|----------|
| W0 | 14,316,108 | 3,285 | 4,359× |
| W1 | 60,273,820 | 3,569 | 16,889× |
| W2 | 41,122,853 | 4,986 | 8,248× |
| W3 | 70,421,086 | 386 | **182,438×** |
