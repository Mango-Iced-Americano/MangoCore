---
title: "页帧、VmPageStore 与页表映射关系"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-07-28
tags: [mm, frame, pagetable, vm-page-store, tlb-batch, smp]
---

# 页帧、VmPageStore 与页表映射关系

## 1. 三层关系

MM 中一个用户虚拟页同时涉及三层状态：

```
VMA 元数据
  └── VmPageStore: VPN -> Frame 状态
        └── FrameTracker / ZramTracker / SwapTracker

页表
  └── VPN -> PPN + PTE flags

物理页分配器
  └── PPN 是否空闲
```

这三层不总是一一对应。比如匿名 lazy 页有 VMA 但没有 frame，也没有 PTE；匿名 shared writable 页可能有 frame 但没有 PTE；文件页可能在 page cache 中，但当前进程没有 PTE。

## 2. Frame 状态表

`os/src/mm/frame_store.rs` 定义 `Frame`：

| 编译特性 | 状态 |
|----------|------|
| 默认 | `InMemory(Arc<FrameTracker>)`, `Unallocated` |
| `oom_handler` | 额外有 `Compressed(Arc<ZramTracker>)`, `SwappedOut(Arc<SwapTracker>)` |

`frame_state()` 根据 VPN 返回 `FrameState`：

| 返回 | 含义 |
|------|------|
| `InMemory` | `VmPageStore` 中有 resident frame |
| `Unallocated` | 没有 frame 或显式未分配 |
| `Compressed` | 页内容在 zram |
| `SwappedOut` | 页内容在 swap |

越界 VPN 返回 `MemoryError::BadAddress`。

源码中的 `Frame` 和 `FrameState` 直接反映 feature 差异：

```rust
#[cfg(feature = "oom_handler")]
#[derive(Clone, Debug)]
pub enum Frame {
    InMemory(Arc<FrameTracker>),
    Compressed(Arc<ZramTracker>),
    SwappedOut(Arc<SwapTracker>),
    Unallocated,
}

#[cfg(not(feature = "oom_handler"))]
#[derive(Clone, Debug)]
pub enum Frame {
    InMemory(Arc<FrameTracker>),
    Unallocated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FrameState {
    InMemory,
    Unallocated,
    #[cfg(feature = "oom_handler")]
    Compressed,
    #[cfg(feature = "oom_handler")]
    SwappedOut,
}
```

因此没有 `oom_handler` 时，文档中提到的 zram/swap 状态不会出现在 `Frame` 枚举中。

## 3. VmPageStore 为什么使用 BTreeMap

`VmPageStore` 字段：

```rust
frames: BTreeMap<VirtPageNum, Frame>
```

源码注释说明使用 BTreeMap 是为了避免 Vec 在大量写入时直接写满堆内存。对于稀疏 VMA，BTreeMap 也避免为每个虚拟页都维护一项。

这与 lazy allocation 配合：VMA 可以很大，但未访问页不占物理页，也不占 `frames` 项。

完整结构如下：

```rust
#[derive(Clone)]
pub struct VmPageStore {
    pub vpn_range: VPNRange,
    frames: BTreeMap<VirtPageNum, Frame>,
    #[cfg(feature = "oom_handler")]
    active: VecDeque<VirtPageNum>,
    #[cfg(feature = "oom_handler")]
    compressed: usize,
    #[cfg(feature = "oom_handler")]
    swapped: usize,
}

impl VmPageStore {
    pub fn try_new(vpn_range: VPNRange) -> Result<Self, isize> {
        Ok(Self {
            vpn_range,
            frames: BTreeMap::new(),
            #[cfg(feature = "oom_handler")]
            active: VecDeque::new(),
            #[cfg(feature = "oom_handler")]
            compressed: 0,
            #[cfg(feature = "oom_handler")]
            swapped: 0,
        })
    }
}
```

`vpn_range` 是合法范围，`frames` 只记录非默认状态。启用 OOM 时，`active/compressed/swapped` 是回收算法的辅助状态。

## 4. 状态组合

常见组合如下：

| VMA | VmPageStore | PTE | 说明 |
|-----|-------------|-----|------|
| anonymous private | `Unallocated` | 无 | lazy 页 |
| anonymous private | `InMemory` | 有 | 已 fault-in |
| anonymous private fork 后 | `InMemory(shared Arc)` | 有但无 W | COW 候选 |
| anonymous shared writable | `InMemory` | 可无 | 预分配 shared frame，懒 PTE |
| file private 读后 | `InMemory(page cache frame)` | 有但无 W | 后续写触发 COW |
| file shared 读后 | `InMemory(page cache frame)` | 有但无 W | 后续写标脏 |
| oom compressed | `Compressed` | 无 | fault 时解压 |
| oom swapped | `SwappedOut` | 无 | fault 时换入 |

因此调试缺页时必须同时看 VMA、`VmPageStore` 和 PTE。

## 5. map_one_unchecked()

普通匿名页映射：

```text
Vma::map_one_unchecked()
  ├── frame_alloc()
  ├── ppn = frame.ppn
  ├── inner.alloc_in_memory(vpn, frame)
  ├── map_page_with_perm(page_table, vpn, ppn, map_perm)
  └── 失败时 remove_in_memory()
```

`map_one_zeroed_unchecked()` 目前同样使用 `frame_alloc()`，因此得到清零页。

## 6. alloc_one_zeroed_unmapped()

该函数只分配 frame 并写入 `VmPageStore`，不安装 PTE：

```rust
let frame = frame_alloc().ok_or(MemoryError::OutOfMemory)?;
self.inner.alloc_in_memory(vpn, frame)?;
```

当前用于 writable anonymous `MAP_SHARED` 预分配。这样 shared backing 已经存在，fork 能共享；PTE 则等首次访问时安装。

frame 写入 `VmPageStore` 的入口会检查 VPN 是否落在范围内，并记录 active 队列：

```rust
pub fn alloc_in_memory(
    &mut self,
    key: VirtPageNum,
    frame: Arc<FrameTracker>,
) -> Result<(), MemoryError> {
    self.check_vpn(key)?;
    self.frames.insert(key, Frame::InMemory(frame));
    #[cfg(feature = "oom_handler")]
    self.record_active(key);
    Ok(())
}
```

这说明 resident frame 的生命周期由 `Arc<FrameTracker>` 持有；PTE 只保存 PPN，不负责物理页所有权。

## 7. map_existing_in_memory()

`ResidentWithoutPte` 缺页调用：

```text
map_existing_in_memory()
  ├── 如果 PTE 已映射 -> AlreadyMapped
  ├── 从 VmPageStore 取 InMemory frame
  ├── map_page_with_perm()
  └── 返回 ppn
```

该路径不会分配新物理页，适合 shared anonymous 预分配页。

## 8. unmap 与 discard

`Vma::unmap()` 删除整段 resident pages：

1. 收集所有 resident VPN。
2. 对每个 VPN 调用 `unmap_user_page_if_mapped()`。
3. 从 `VmPageStore` 删除 resident frame。

`discard_range()` 只删除指定范围，主要用于 `MADV_DONTNEED`。它也只遍历 resident VPN，不会为 lazy 页做额外工作。

如果 unmap 某页时 PTE 已不存在，代码会记录 warning，但继续清理 VMA 元数据。这适配 lazy alloc 和 OOM unmap 后的状态。

## 9. VmPageStore 分裂

VMA 分裂调用 `VmPageStore::into_two(cut)`：

| 操作 | 说明 |
|------|------|
| `frames.split_off(&cut)` | 后半段 frame 状态移到新 store |
| `vpn_range` 更新 | 原 store 截断，新 store 从 cut 开始 |
| active 队列拆分 | `oom_handler` 下按 VPN 归属拆分 |
| compressed/swapped 重算 | 避免计数漂移 |

这保证 `mprotect/munmap/madvise` 分裂 VMA 后，每个子 VMA 的页状态仍与 VPN 范围一致。

## 10. 页表映射接口

VMA 通过 `UserMapper` 修改页表：

| 方法 | 用途 |
|------|------|
| `map_user_page()` | 映射用户页，要求 `U` |
| `map_privileged_user_page()` | 映射非用户页，如 trap context |
| `unmap_user_page()` | 删除用户页映射 |
| `unmap_user_page_if_mapped()` | 若存在则删除 |
| `set_user_flags()` | 更新 PTE 权限，要求 `U` |
| `set_ppn()` | 更新 PTE 物理页 |

`map_page_with_perm()` 根据 flags 是否包含 `U` 选择 user 或 privileged 映射。

## 11. COW 中的 frame 与 PTE 同步

COW 复制时同步顺序很重要：

```text
分配 new_frame
  ├── copy old_ppn -> new_ppn
  ├── VmPageStore old -> new
  ├── page_table.set_ppn(vpn, new_ppn)
  └── page_table.set_pte_flags(vpn, map_perm)
```

如果先改 PTE 再更新 `VmPageStore`，中途失败会让页表指向无 owner 的页；如果先丢旧 frame 再无法设置 PTE，会丢失旧数据。`Vma::copy_on_write()` 对这些失败点做了回滚。

## 12. 文件 page cache frame

文件 mmap 的 resident frame 可能来自 page cache：

| 路径 | Frame 来源 |
|------|------------|
| `filemap_read_fault()` | `pc.frame_for_read()` |
| `filemap_shared_write_fault()` | `pc.frame_for_write()` |
| `filemap_private_fault()` | 新分配 private frame，内容来自 page cache |

这意味着 `VmPageStore` 中的 `Arc<FrameTracker>` 不一定是匿名私有页，也可能是文件系统 page cache 共享页。

## 13. 页表释放

地址空间释放分两层：

1. `vmas.unmap_all(batch)` 先清除 resident PTE，batch 刷新 TLB 后再释放 VMA frame。
2. `page_table.release_frames()` 随后释放页表自身持有的页表页。

不能直接 drop VMA frame：本 CPU 的旧 TLB 可能仍持有对应 PPN。仅释放页表页
也不能 drop 用户页帧引用；两层必须按上述顺序执行。

## 14. TLB 同步

页表 PTE 修改必须配套 TLB 失效。用户路径的批量边界是 `TlbBatch`，
fork 同时使用父、子两个 batch：

```rust
if let Some(ppn) = src_batch.block_write(vpn) {
    UserMapper::new(dst_batch).map_user_page(vpn, ppn, map_perm)?;
}
// AddressSpace::from_existing_user() 在遍历后提交两个 batch。
```

`set_ppn`、`set_user_flags`、`unmap` 等用户 PTE 修改同样经 batch；从 VMA
移出的 frame 由 `defer_frame()` 保留到 flush 之后。内核页表仍使用带当场刷新的
`PageTable` 安全接口。

上述“flush 之后”当前只对 `Unpublished/LocalOnly` 完整成立。B22 已提供每 MM 的
cached CPU/generation 激活状态和远端全用户 IPI/ack 原语，但 `Published` batch 仍
fail-stop；B23 必须把 deferred frame 移到 VM 锁外，并在全部远端 ack 后才释放。

Frame 和 PTE 的对应关系不是自动维护的。`VmPageStore` 持有 `FrameTracker`，保证物理页生命周期；PTE 持有 PPN，供硬件翻译。正确状态要求两边同时指向同一页：如果 PTE 指向一个没有 `FrameTracker` 引用的页，页可能被 allocator 回收后重用；如果 `VmPageStore` 有 frame 但 PTE 没有映射，页面只 resident，不可被用户直接访问，可能是 lazy/shared anonymous 或被暂时撤销权限的状态。

释放地址空间也要分两步：先经 batch unmap/flush/drop 用户 frame，再
release page table 释放页表页。只做前者会泄漏页表页，只做后者会让用户
frame 的 `Arc` 继续存在。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| PTE 指向页但 VMA 查不到 frame | `VmPageStore` 更新失败或 stale lazy PTE |
| VMA 有 frame 但访问缺页 | shared anonymous 预分配或 OOM unmap 后状态 |
| unmap 后 frame 泄漏 | `VmPageStore::remove_in_memory()` 是否执行 |
| VMA 分裂后访问错页 | `VmPageStore::into_two()` 是否按 cut 分离 |
| COW 数据丢失 | `set_ppn`/`set_user_flags` 失败回滚 |
