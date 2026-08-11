---
title: "物理页分配器与 FrameTracker"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-03
tags: [mm, frame, allocator, oom]
code_paths:
  - "os/src/mm/frame_allocator.rs"
  - "os/src/hal/firmware/"
  - "os/src/hal/arch/loongarch64/config.rs"
  - "os/src/hal/arch/riscv/config.rs"
---

# 物理页分配器与 FrameTracker

## 1. 源码位置

物理页分配由 `os/src/mm/frame_allocator.rs` 实现，并被 `os/src/mm/mod.rs` 导出。

| 接口 | 作用 |
|------|------|
| `init_frame_allocator()` | 初始化全局页帧分配器 |
| `frame_alloc()` | 分配一个清零页帧，返回 `Arc<FrameTracker>` |
| `frame_alloc_uninit()` | 分配一个未清零页帧，调用者必须立即完整写入 |
| `frames_alloc(count)` | 在单一 DRAM region 内分配物理连续页，供 DMA 使用 |
| `frames_alloc_any(count)` | 批量分配不要求物理连续的页，供页表映射使用 |
| `frame_reclaim_linker_range()` | 在最后一次读取后显式回收链接器内嵌载荷的完整页 |
| `frame_dealloc(ppn)` | 释放指定物理页号 |
| `unallocated_frames()` | 返回当前未分配页帧数量 |
| `try_unallocated_frames()` | panic 中尝试读取余量；写锁忙时立即返回 `None` |
| `frag_diagnostic()` | 输出碎片化诊断数据 |
| `frame_reserve(pages)` | 在 OOM handler 特性下尝试预留页 |

页帧状态在 VMA 层由 `os/src/mm/frame_store.rs::VmPageStore` 记录。本页聚焦全局物理页分配器和 `FrameTracker` 的生命周期。

## 2. 分配器模型

实现中的分配器为“每个 DRAM region 一个 fresh 游标，共享一个回收栈”：

```rust
struct FrameRegion {
    start: usize,
    current: usize,
    end: usize,
    recycled_flags: Vec<bool>,
}

pub struct StackFrameAllocator {
    regions: Vec<FrameRegion>,
    reclaimed_regions: Vec<ReclaimedRegion>,
    fresh_region: usize,
    recycled: Vec<usize>,
}
```

字段语义：

| 字段 | 含义 |
|------|------|
| `regions` | 从平台 DRAM 表扣除第 0 页、内核镜像和固件 carveout 后的可分配区间 |
| `fresh_region` | 第一个尚未耗尽的 fresh region 索引 |
| `recycled` | 已释放、可复用的 PPN 栈 |
| `recycled_flags` | 各 region 内 O(1) 重复释放检测标记 |
| `reclaimed_regions` | 已完成复制、由链接器所有权转交给分配器的 payload 页 |

分配策略为：

1. 优先从 `recycled` 弹出之前释放的页帧。
2. 如果没有 recycled 页，则按 region 顺序从 `current` 线性递增分配。
3. region 耗尽后切到下一个 region，绝不跨越地址空洞。
4. 多页连续分配先寻找同一 region 内的连续 recycled extent，再使用单一 fresh extent。

## 3. 初始化范围

QEMU 的权威内存拓扑来自 BSP 在清 BSS 前冻结的 FDT：
`hal::firmware::memory_regions()` 描述 DRAM bank，
`firmware_reserved_regions()` 合并板级必需 carveout、FDT memreserve、
`/reserved-memory` 与原始 DTB 页面。帧分配器不再使用 QEMU 的编译期容量常量。
2K1000LA 在 U-Boot 没有提供合法 EFI/FDT 时，仍把经过实板验证的静态双 bank
填入同一固件资源表，因此下游 MM 不维护第二套分配路径：

| 类型 | 物理范围 | 处理 |
|------|----------|------|
| DRAM bank 0 | `[0x00000000, 0x10000000)` | 第 0 页和固件 carveout 不分配 |
| MMIO/空洞 | `[0x10000000, 0x90000000)` | 不进入 frame allocator |
| DRAM bank 1 | `[0x90000000, 0x100000000)` | 从 `ekernel` 后开始分配 |
| 临时 carveout | `[0x0cbf4000, 0x10000000)` | U-Boot、DVO framebuffer、CPU1 park loop、BPI/SMBIOS；完成所有权交接前保留 |

`MEMORY_SIZE=2 GiB` 表示实板安装容量，`MEMORY_END=4 GiB` 是静态 fallback 的最高
物理地址；二者都不能代替运行期 region 表。`/proc/meminfo`、`sysinfo(2)` 和 RamFS
`statfs` 使用 `firmware::usable_memory_size()`，避免把固件 carveout 报告为可用内存。
初始化会对每个 DRAM bank 逐一减去：

1. 物理第 0 页，避免空指针与 LA64 页表 token 语义冲突。
2. `[skernel, ekernel)` 内核镜像。
3. `FIRMWARE_RESERVED_REGIONS` 中仍有外部所有者的区间。

RISC-V OpenSBI 平台另外固定保留 `[0x80000000, 0x80200000)`。该板级所有权不会
因为 QEMU FDT 未列出 reserved-memory 而丢失，而是先加入固件资源表，再和动态保留区
统一合并。内核链接与入口位于 `0x80200000`；低端 2 MiB 既不进入 frame allocator，
也不参与启动期批量清零。

固件保留区和内核镜像允许重叠且不要求预先排序。`for_each_usable_ram_range()` 对
memory 起止向内取整页、对 exclusion 向外取整页，并以无堆算法计算区间并集；这样既能
处理 DTB 落入 kernel BSS 的情况，也不会在 LA64 不连续内存的 MMIO hole 上做清零或分配。

## 4. FrameTracker 生命周期

`FrameTracker` 是物理页的 RAII 句柄：

```rust
pub struct FrameTracker {
    pub ppn: PhysPageNum,
}
```

`Drop` 实现会调用 `frame_dealloc(self.ppn)`。因此，页帧的所有权通常表现为 `Arc<FrameTracker>`：

| 持有者 | 说明 |
|--------|------|
| `Vma.inner: VmPageStore` | 用户 VMA 的匿名页、共享页、文件页缓存引用 |
| `KernelMappingArea` | 动态内核映射页 |
| PageCache | 文件页缓存中的物理页 |
| 页表实现内部 | 页表页由具体架构页表实现持有 |

当最后一个 `Arc<FrameTracker>` 被释放时，物理页回到全局分配器。

普通单页分配在发布 `FrameTracker` 前会经过短命的
`FrameReservation`：它表示 PPN 已从 fresh/recycled 元数据中唯一领取，
但页还没有交给上层。`into_tracker()` 使用 `Option::take()` 先完成回收
责任移交，再返回 `FrameTracker`；未消费的 reservation 则由 `Drop` 归还 PPN。
这个中间 owner 只用于分开全局锁与页初始化，不会暴露到 MM 公开 API。

## 5. 清零与未初始化分配

普通 `FrameTracker::new(ppn)` 会清零整页：

```rust
let ptr = ppn.start_addr().direct_map_ptr().cast::<u64>();
for word in 0..PAGE_SIZE / core::mem::size_of::<u64>() {
    unsafe { ptr.add(word).write(0) };
}
```

这对用户匿名页、内核动态页和安全隔离都很关键。新分配给用户空间的页面不能包含旧进程或内核路径残留的数据。
实际实现继续按 8 个 `u64` 手工展开，不改变既有清零吞吐。B88 删除了只为该调用点服务、
却能返回 `'static mut [u64]` 的通用 helper；raw pointer 只在页已从 fresh/recycled 集合摘除、
尚未发布 `FrameTracker` 的独占窗口内存在。

未初始化路径只在明确安全的场景使用：

```rust
pub unsafe fn frame_alloc_uninit() -> Option<Arc<FrameTracker>>
```

当前典型用途是 COW：

1. 写缺页确认需要复制。
2. 分配未初始化新页。
3. 立即从旧页复制完整 `PAGE_SIZE` 字节。
4. 更新 `VmPageStore` 和 PTE。

如果新增调用点不能证明会完整覆盖整页，就不能使用 `frame_alloc_uninit()`。

## 6. 分配路径

`frame_alloc()` 的主路径：

```text
frame_alloc()
  ├── lock FRAME_ALLOCATOR
  ├── reserve_one()
  │     ├── prezeroed.pop()
  │     ├── recycled.pop()
  │     └── regions[fresh_region].current += 1
  ├── unlock FRAME_ALLOCATOR
  ├── FrameReservation::into_tracker()
  │     └── zero 4 KiB when required
  └── Arc::new(FrameTracker)
```

B89 之前，单页的 4 KiB 清零位于 `FRAME_ALLOCATOR` 全局写锁内；8 核同时
缺页时，不相关 PPN 的初始化也被该锁完全串行化。现在锁内只修改
free-list/region cursor 和 owner bit，之后的页写入与 `Arc` 构造都在锁外完成。
OOM 首次尝试和重试也都显式用局部变量接住 reservation，依赖语句结束
释放 guard，不依赖链式临时值的延长规则。

`frames_alloc()`、`frames_alloc_fresh_contiguous()` 和显式 `frame_alloc_uninit()` 仍保持
原有路径：连续区间的选择/回滚需作为独立所有权协议设计，不为了扩大
B89 而把它们混入单页 reservation。

若启用 `zero_init` 特性，BSP 会在建堆前沿同一个动态 usable-region 迭代器清零所有
未来 fresh 页，并跳过内核、固件 carveout 和内存洞；fresh 页随后可走 `new_uninit`
快路径。这属于编译特性控制，不改变 `frame_alloc()` 对调用者暴露的所有权模型。

### 6.1 Idle 预清零池与运行时 A/B

调度器空闲路径可以调用 `idle_prezero_refill()`，每个 idle tick 最多领取并清零 2 页，
池高水位为 256 页。领取和发布只在短暂持有 `FRAME_ALLOCATOR` 锁时完成，4 KiB 清零
位于锁外；低于 2048 个空闲页时停止补充，避免预清零放大内存压力。

启动参数 `mango.mm.prezero=` 控制同一内核二进制的 A/B：

- `idle`（默认）：CPU 进入 idle 路径时允许有界补充；
- `quiescent`：仅当全局没有 ready/current task 时补充；
- `off`：完全关闭补充。

普通 `frame_alloc()` 优先消费预清零页，池空时仍按 recycled/fresh 路径满足 demand
allocation。可选的匿名 fault-around 则调用 `try_frame_alloc_prezeroed()`：该接口只消费
已经清零的池页，绝不回退到同步清零、fresh allocation 或 OOM recovery。这样错误预测
最多浪费有界 idle 工作，不会把额外延迟引入真正的缺页关键路径。

评估预清零本身时应关闭 fault-around，仅比较 `prezero=off` 与 `prezero=idle`；评估
fault-around 时两组都固定 `prezero=idle`。`/sys/kernel/stats/pagefault` 导出当前策略、
池命中/未命中、补充页数/耗时及策略/活跃任务跳过次数。

## 7. 释放路径与重复释放防御

`frame_dealloc(ppn)` 会检查：

1. `ppn` 是否属于某个已推进到该页的 fresh region，或显式登记的 reclaimed region。
2. `ppn` 是否已经在 recycled 集合中。
3. 若合法，则压入 `recycled` 并设置 `recycled_flags`。

重复释放是严重内存破坏，分配器用 `recycled_flags` 做 O(1) 检测，而不是遍历 `recycled` 向量。这一点对高频页释放路径更稳定。

`is_allocatable_ram_phys_addr()` 是 fault/uaccess 使用的无锁物理拓扑后验检查：它确认整页
落在一个固件 DRAM bank 中、不属于第 0 页，也不和固件保留区重叠；当前页究竟由哪个 VMA
或 `FrameTracker` 持有，仍由上层生命周期保证。它不能永久排除 `[skernel, ekernel)`，因为
linker payload 的完整页在复制后仍保留原物理地址，却已由
`frame_reclaim_linker_range()` 正式转交。把 allocator 的 `RwLock` 引入每页 uaccess 也不可取，
会让用户复制热路径与帧分配产生无谓竞争。

## 8. 与 VmPageStore 的关系

全局分配器只知道“物理页是否空闲”。某个用户虚拟页的状态由 `VmPageStore` 维护：

```rust
pub enum Frame {
    InMemory(Arc<FrameTracker>),
    Unallocated,
    #[cfg(feature = "oom_handler")]
    Compressed(Arc<ZramTracker>),
    #[cfg(feature = "oom_handler")]
    SwappedOut(Arc<SwapTracker>),
}
```

因此，一个 VPN 的状态可能是：

| 状态 | PTE | 物理页 |
|------|-----|--------|
| `Unallocated` | 无有效 PTE | 未分配 |
| `InMemory` 且已映射 | 有有效 PTE | `Arc<FrameTracker>` |
| `InMemory` 但未映射 | 无有效 PTE | 共享匿名页预分配等场景 |
| `Compressed` | 无有效 PTE | 数据在 zram |
| `SwappedOut` | 无有效 PTE | 数据在 swap |

缺页处理根据 `VmPageStore` 状态决定是 lazy alloc、map resident page、decompress 还是 swap in。

## 9. 批量分配

`frames_alloc(count)` 返回一个物理连续 extent，且保证全部页面位于同一个 DRAM region。
它优先复用连续 recycled 页；找不到时才从某个 region 的 fresh 尾部一次性取出。
这使 VirtIO 把首个 PA 当成线性 DMA 缓冲时不会跨入 2K1000LA 的 MMIO 空洞，也不会因只消耗 fresh 页而在长期 I/O 后假性耗尽。

`frames_alloc_any(count)` 通过多次 `frame_alloc()` 构建不连续页集合，适用于 SysV SHM 等由页表建立逻辑连续性的场景。两者不能互换。

链接器嵌入的 initramfs、initproc、bash、busybox 等位于 `ekernel` 之前，不能伪装成普通 `frame_dealloc()`。复制完成后，调用方以 unsafe 契约调用 `frame_reclaim_linker_range()`；分配器检查其位于单一 DRAM bank、不与 fresh region、既有 reclaimed region 或固件 carveout 重叠，再登记生命周期。尾部不完整页保持归内核所有。

## 10. OOM handler 配合

在启用 `oom_handler` 特性时，分配失败路径会尝试回收：

```text
frame_alloc()
  ├── allocator.alloc()
  ├── if none:
  │     ├── oom_handler(1)
  │     └── retry allocator.alloc()
  └── return Option<Arc<FrameTracker>>
```

`frame_reserve(pages)` 同样在该特性下调用 `oom_handler(pages)`；未启用时为空操作。上层在缺页和 uaccess fault-in 前调用 `frame_reserve(3)`，是为了在真正分配页表页、数据页或元数据前预留恢复空间。

## 11. 碎片化诊断

`frag_diagnostic()` 返回当前分配器碎片信息。配合 `unallocated_frames()` 可以判断问题是页总量耗尽，还是 recycled 页分布、堆元数据或其他资源造成的间接 OOM。

`heap_stats()` 不属于物理页分配器，它统计内核堆 buddy allocator 的状态。排查 OOM 时二者要分开看：

| 指标 | 来源 | 表示 |
|------|------|------|
| `unallocated_frames()` | frame allocator | 物理页余量 |
| `heap_stats()` | heap allocator | 内核堆余量和内部浪费 |
| `committed_as_kbytes()` | sysctl | 当前进程地址空间承诺量 |

物理页分配器和内核堆是两套资源。`FrameTracker` 管 4 KiB 物理页，主要服务页表、用户页、PageCache 和 DMA；heap allocator 管内核对象内存，服务 `Arc`、`Vec`、`BTreeMap` 等元数据。出现 `ENOMEM` 时必须判断失败点在哪一层：frame 充足但 `Vec::try_reserve()` 失败是堆问题，heap 充足但 `frame_alloc()` 返回 None 是物理页问题。

普通诊断允许使用上述阻塞统计；panic 路径必须改用 `try_unallocated_frames()` 和
`try_heap_stats()`。锁忙本身就是有价值的崩溃现场信息，此时打印 `<locked>` 或原子 heap
charge，不能为了取得一个更精确数字再次等待可能永不释放的 allocator owner。

`FrameTracker` 的 RAII 语义是防止双重释放和悬空物理页的核心。VMA、PageCache、shared anonymous frame 都通过 `Arc<FrameTracker>` 共享页帧；只有最后一个引用 drop 时，页帧才回到 allocator。调试“页被提前复用”时优先检查是否有裸 PPN 绕开 `FrameTracker` 生命周期。

## 12. 关键约束

1. `frame_alloc_uninit()` 只能在立即整页覆盖的路径使用。
2. 不能手动调用 `frame_dealloc()` 释放仍被 `Arc<FrameTracker>` 持有的页。
3. 页帧释放依赖 `FrameTracker` drop，不应复制裸 `ppn` 后绕开 RAII。
4. fork COW 中的 `Arc::strong_count()` 判断依赖 `VmPageStore` 对页帧引用的准确性。
5. shared anonymous 预分配会持有 `Arc<FrameTracker>`，即使 PTE 暂时未安装。
6. OOM recovery 只能改善页帧可用性，不保证内核堆元数据分配一定成功。
7. DRAM 总量、物理地址上界与可分配区间是三个不同概念；有地址空洞的平台必须遍历 `MEMORY_REGIONS`。
8. DMA 连续页必须由 `frames_alloc()` 分配，不能假设多次单页分配所得 PPN 连续。
9. 固件 carveout 只有在设备 DMA 停止、其他 CPU 重停放、启动参数复制完成后才能显式释放。
10. 物理页 raw pointer 只能在能证明 allocator/owner 独占的局部作用域解引用，不能重新包装为
    可从安全函数取得的 `'static mut` 引用。

## 13. 2K1000LA 实板验收

2026-07-13 的实板门禁覆盖了以下路径：

1. 启动探针确认低 bank 可用区为 `[0x1000, 0x0cbf4000)`，高 bank 从
   `ekernel` 延伸到 `0x100000000`，两者之间的 MMIO 空洞和顶部 carveout 均未进入分配器。
2. 在 RamFS 写入并校验 320 MiB 零文件，日志出现 `region0 -> region1`；文件长度为
   `335544320`，校验和为 `2699711059`，删除后 `MemFree` 恢复到测试前仅差一页。
3. SATA 只读探针从低 bank 取得 AHCI DMA 页，重复读取 LBA0 一致并验证 MBR
   `55aa`；QEMU VirtIO PCI 快照启动可挂载 `/dev/vda` Ext4 并持续运行 LTP，无 panic。
4. ABI 统计报告 `MemTotal: 2043852 kB`，同时 `MEMORY_SIZE` 仍保留已安装容量 2 GiB。

## 14. 调试核对点

| 现象 | 核对路径 |
|------|----------|
| 新匿名页包含旧数据 | 是否误用了 `frame_alloc_uninit()` |
| COW 后父子互相污染 | `VmPageStore` 是否共享同一个 `Arc<FrameTracker>` 且 PTE W 权限处理是否正确 |
| 释放时报 duplicate panic | 是否存在手动 `frame_dealloc` 与 RAII drop 双重释放 |
| `mmap` 大量小映射后 ENOMEM | 同时检查页帧、内核堆和 `max_map_count` |
| shared anonymous mincore 异常 | 预分配页在 `VmPageStore` 中，但 PTE 懒安装，需按代码语义判断 |
