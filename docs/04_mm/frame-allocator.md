---
title: "物理页分配器与 FrameTracker"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, frame, allocator, oom]
---

# 物理页分配器与 FrameTracker

## 1. 源码位置

物理页分配由 `os/src/mm/frame_allocator.rs` 实现，并被 `os/src/mm/mod.rs` 导出。

| 接口 | 作用 |
|------|------|
| `init_frame_allocator()` | 初始化全局页帧分配器 |
| `frame_alloc()` | 分配一个清零页帧，返回 `Arc<FrameTracker>` |
| `frame_alloc_uninit()` | 分配一个未清零页帧，调用者必须立即完整写入 |
| `frames_alloc(count)` | 批量分配页帧 |
| `frame_dealloc(ppn)` | 释放指定物理页号 |
| `unallocated_frames()` | 返回当前未分配页帧数量 |
| `frag_diagnostic()` | 输出碎片化诊断数据 |
| `frame_reserve(pages)` | 在 OOM handler 特性下尝试预留页 |

页帧状态在 VMA 层由 `os/src/mm/frame_store.rs::VmPageStore` 记录。本页聚焦全局物理页分配器和 `FrameTracker` 的生命周期。

## 2. 分配器模型

实现中的分配器是栈式页帧分配器：

```rust
pub struct StackFrameAllocator {
    start: usize,
    current: usize,
    end: usize,
    recycled: Vec<usize>,
    recycled_flags: Vec<bool>,
}
```

字段语义：

| 字段 | 含义 |
|------|------|
| `start` | 可分配物理页号起点，用于把 PPN 映射到 `recycled_flags` 下标 |
| `current` | 尚未线性分配的下一个 PPN |
| `end` | 可分配 PPN 的结束边界 |
| `recycled` | 已释放、可复用的 PPN 栈 |
| `recycled_flags` | O(1) 重复释放检测标记 |

分配策略为：

1. 优先从 `recycled` 弹出之前释放的页帧。
2. 如果没有 recycled 页，则从 `current` 线性递增分配。
3. `current == end` 时无页可分配。

## 3. 初始化范围

初始化入口：

```rust
pub fn init_frame_allocator() {
    extern "C" {
        fn ekernel();
    }
    FRAME_ALLOCATOR.exclusive_access().init(
        PhysAddr::from(ekernel as usize).ceil(),
        PhysAddr::from(MEMORY_END).floor(),
    );
}
```

范围含义：

| 边界 | 来源 | 说明 |
|------|------|------|
| start | `ekernel` 向上取整 | 内核镜像结束后的第一页 |
| end | `MEMORY_END` 向下取整 | 平台配置的物理内存结束 |

这保证物理页分配器不会把内核代码、数据、BSS、启动栈所在内存重新分配给用户页或页表页。

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

## 5. 清零与未初始化分配

普通 `FrameTracker::new(ppn)` 会清零整页：

```rust
let bytes = ppn.get_bytes_array();
for chunk in bytes.chunks_exact_mut(core::mem::size_of::<u64>()) {
    chunk.copy_from_slice(&0u64.to_ne_bytes());
}
```

这对用户匿名页、内核动态页和安全隔离都很关键。新分配给用户空间的页面不能包含旧进程或内核路径残留的数据。

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
  ├── alloc()
  │     ├── recycled.pop()
  │     └── current += 1
  ├── FrameTracker::new(ppn)
  └── Arc::new(FrameTracker)
```

若启用 `zero_init` 特性，fresh 页可能走 `new_uninit` 优化；这属于编译特性控制，不改变 `frame_alloc()` 对调用者暴露的所有权模型。

## 7. 释放路径与重复释放防御

`frame_dealloc(ppn)` 会检查：

1. `ppn` 是否在可分配范围内。
2. `ppn` 是否已经在 recycled 集合中。
3. 若合法，则压入 `recycled` 并设置 `recycled_flags`。

重复释放是严重内存破坏，分配器用 `recycled_flags` 做 O(1) 检测，而不是遍历 `recycled` 向量。这一点对高频页释放路径更稳定。

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

`frames_alloc(count)` 返回 `Vec<Arc<FrameTracker>>`。它通过多次 `frame_alloc()` 构建结果，一旦中途失败，已经放入 `Vec` 的 `FrameTracker` 会随 `Vec` drop 释放。

该接口适用于需要一次性准备连续数量页对象但不要求物理连续的场景。当前 MM 代码没有把它作为连续物理内存分配器使用。

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

`FrameTracker` 的 RAII 语义是防止双重释放和悬空物理页的核心。VMA、PageCache、shared anonymous frame 都通过 `Arc<FrameTracker>` 共享页帧；只有最后一个引用 drop 时，页帧才回到 allocator。调试“页被提前复用”时优先检查是否有裸 PPN 绕开 `FrameTracker` 生命周期。

## 12. 关键约束

1. `frame_alloc_uninit()` 只能在立即整页覆盖的路径使用。
2. 不能手动调用 `frame_dealloc()` 释放仍被 `Arc<FrameTracker>` 持有的页。
3. 页帧释放依赖 `FrameTracker` drop，不应复制裸 `ppn` 后绕开 RAII。
4. fork COW 中的 `Arc::strong_count()` 判断依赖 `VmPageStore` 对页帧引用的准确性。
5. shared anonymous 预分配会持有 `Arc<FrameTracker>`，即使 PTE 暂时未安装。
6. OOM recovery 只能改善页帧可用性，不保证内核堆元数据分配一定成功。

## 13. 调试核对点

| 现象 | 核对路径 |
|------|----------|
| 新匿名页包含旧数据 | 是否误用了 `frame_alloc_uninit()` |
| COW 后父子互相污染 | `VmPageStore` 是否共享同一个 `Arc<FrameTracker>` 且 PTE W 权限处理是否正确 |
| 释放时报 duplicate panic | 是否存在手动 `frame_dealloc` 与 RAII drop 双重释放 |
| `mmap` 大量小映射后 ENOMEM | 同时检查页帧、内核堆和 `max_map_count` |
| shared anonymous mincore 异常 | 预分配页在 `VmPageStore` 中，但 PTE 懒安装，需按代码语义判断 |
