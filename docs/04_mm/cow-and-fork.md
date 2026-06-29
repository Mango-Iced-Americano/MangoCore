---
title: "fork 地址空间复制与 COW"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, fork, cow, clone]
---

# fork 地址空间复制与 COW

## 1. 入口链路

fork/clone 的进程地址空间复制由 task/process 层发起，MM 实现入口在 `os/src/mm/address_space.rs::AddressSpace::from_existing_user()`。

```
sys_clone / fork 语义
  └── task clone path
        └── AddressSpace::from_existing_user(user_space, trap_cx_slot, trap_cx)
              ├── AddressSpace::new_bare()
              ├── map_trampoline()
              ├── map_signaltrampoline()
              ├── 复制 heap_bottom / heap_pt
              ├── 遍历 user_space.vmas
              │     ├── dont_fork -> 跳过
              │     ├── wipe_on_fork -> Vma::from_another()
              │     └── try_clone() + map_from_existing_page_table()
              ├── mark_fork_inherited()
              └── 复制 trap context VMA
```

该路径只处理地址空间。文件描述符、信号、PID 等由 process/task 模块处理。

## 2. 实现状态

| 功能 | 状态 |
|------|------|
| private writable fork CoW | fork 时父子 PTE 去 W，写缺页进入 `copy_on_write()` |
| shared anonymous fork | 共享 `Arc<FrameTracker>`，首次访问按 `ResidentWithoutPte` 安装 PTE |
| shared file mapping | 不做 CoW，写路径通过 PageCache 标脏 |
| `MADV_DONTFORK` | fork 时跳过对应 VMA |
| `MADV_WIPEONFORK` | 继承 VMA 形状但清空 resident pages |
| 父页表 TLB 刷新 | 批量撤 W 后统一 `flush_tlb()` |

## 3. VMA 继承规则

fork 遍历父进程 VMA 时有三个分支：

| VMA 标记 | 子进程行为 |
|----------|------------|
| `dont_fork = true` | 不继承 |
| `wipe_on_fork = true` | 继承 VMA 形状和权限，但不继承 resident pages |
| 默认 | 继承 VMA 和 resident frame 引用 |

`dont_fork` 和 `wipe_on_fork` 来自 `madvise()`：

| advice | 字段 |
|--------|------|
| `MADV_DONTFORK` | `dont_fork = true` |
| `MADV_DOFORK` | `dont_fork = false` |
| `MADV_WIPEONFORK` | `wipe_on_fork = true` |
| `MADV_KEEPONFORK` | `wipe_on_fork = false` |

`MADV_WIPEONFORK` 只允许 anonymous private VMA。

## 4. Vma::try_clone()

普通 VMA 复制先调用 `try_clone()`：

```rust
let inner = self.inner.try_clone()?;
```

`VmPageStore::try_clone()` 会复制 `frames: BTreeMap<VirtPageNum, Frame>`。其中 `Frame::InMemory(Arc<FrameTracker>)` 的 clone 会增加 `Arc` 引用计数，而不是复制物理页内容。

这就是 COW 的页帧共享基础。

## 5. map_from_existing_page_table()

复制 VMA 后，子进程页表映射由 `Vma::map_from_existing_page_table(dst, src)` 建立。

关键判断：

```rust
let is_shared = self.flags.contains(MapFlags::MAP_SHARED);
let is_file_backed = self.map_file.is_some();
let is_writable = self.map_perm.contains(MapPermission::W);
let protect_parent_for_cow = !is_shared && is_writable;
```

映射权限选择：

| VMA 类型 | 子 PTE 权限 | 父 PTE 处理 |
|----------|-------------|-------------|
| shared file writable | 去掉 W | 不撤销父 W，仅后续 shared write fault 标脏 |
| private writable | 去掉 W | 撤销父 W |
| 非 writable 或其他 shared | 原权限 | 不修改父 PTE |

private writable 是 COW 的核心场景。

`map_from_existing_page_table()` 的源码如下：

```rust
pub fn map_from_existing_page_table<T: PageTable>(
    &mut self,
    dst_page_table: &mut T,
    src_page_table: &mut T,
) -> Result<(), MemoryError> {
    let is_shared = self.flags.contains(MapFlags::MAP_SHARED);
    let is_file_backed = self.map_file.is_some();
    let is_writable = self.map_perm.contains(MapPermission::W);
    let protect_parent_for_cow = !is_shared && is_writable;
    let map_perm = if is_shared && is_file_backed && is_writable {
        self.map_perm.difference(MapPermission::W)
    } else if protect_parent_for_cow {
        self.map_perm.difference(MapPermission::W)
    } else {
        self.map_perm
    };
    let mut parent_tlb_dirty = false;
    let mut first_error = None;
    let mut dst_mapper = UserMapper::new(dst_page_table);
    for vpn in self.inner.vpn_range {
        let ppn = if protect_parent_for_cow {
            let ppn = src_page_table.block_and_ret_mut_no_flush(vpn);
            parent_tlb_dirty |= ppn.is_some();
            ppn
        } else {
            src_page_table.translate(vpn)
        };
        if let Some(ppn) = ppn {
            if !dst_mapper.is_mapped(vpn) {
                let map_result = if map_perm.contains(MapPermission::U) {
                    dst_mapper.map_user_page(vpn, ppn, map_perm)
                } else {
                    dst_mapper.map_privileged_user_page(vpn, ppn, map_perm)
                };
                if let Err(err) = map_result {
                    first_error = Some(err);
                    break;
                }
            } else {
                first_error = Some(MemoryError::AlreadyMapped);
                break;
            }
        }
    }
    if parent_tlb_dirty {
        src_page_table.flush_tlb();
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
```

这段函数有两个权限策略。private writable 映射通过 `block_and_ret_mut_no_flush()` 撤销父 PTE 的 W 权限，并把子 PTE 也映射成无 W；file-backed shared writable 子 PTE 同样去 W，但目的不是 COW，而是让首次 store 进入 shared dirty 路径。

## 6. 父页表批量刷新

private writable fork 时，父页表使用：

```rust
let ppn = src_page_table.block_and_ret_mut_no_flush(vpn);
parent_tlb_dirty |= ppn.is_some();
```

遍历结束后：

```rust
if parent_tlb_dirty {
    src_page_table.flush_tlb();
}
```

这样避免每页撤销 W 都单独 flush。必须保证函数返回前完成刷新，否则父进程可能仍通过旧 TLB 写共享页。

## 7. 子进程映射建立

对父页表中已有 PPN 的页，子进程安装同一 PPN：

```rust
dst_mapper.map_user_page(vpn, ppn, map_perm)
```

如果父 VMA 中某页仍是 lazy unallocated，没有 PTE，也不会给子进程安装 PTE。子进程后续访问时按自己的 VMA 和 `VmPageStore` 状态触发 lazy alloc 或 map resident page。

## 8. 写缺页进入 COW

fork 后，父子任一方写 private writable 页：

```
store page fault
  └── PageFaultHandler::classify()
        ├── PTE 已映射
        ├── access = Store
        ├── VMA 非 shared
        └── FaultAction::Cow
              └── Vma::copy_on_write()
```

如果 PTE 不存在，则不会走 COW，而是根据 VMA 类型和 `VmPageStore` 状态走 lazy alloc、file backed write 或 resident mapping。

## 9. copy_on_write()

COW 复制逻辑：

```text
copy_on_write(page_table, vpn)
  ├── cow_source_frame()
  ├── if Arc::strong_count(old_frame) <= 2:
  │     ├── set_user_flags(vpn, self.map_perm)
  │     └── 返回旧 ppn
  └── else:
        ├── frame_alloc_uninit()
        ├── copy old page -> new page
        ├── VmPageStore: old frame -> new frame
        ├── set_ppn(vpn, new_ppn)
        ├── set_user_flags(vpn, self.map_perm)
        └── 返回 new_ppn
```

`<= 2` 的原因是 `cow_source_frame()` 返回了一个 cloned `Arc`，因此独占页在函数内至少有两个引用：VMA 中一个，本地临时变量一个。

`Vma::copy_on_write()` 的真实实现如下：

```rust
pub fn copy_on_write<T: PageTable>(
    &mut self,
    page_table: &mut T,
    vpn: VirtPageNum,
) -> Result<PhysPageNum, MemoryError> {
    let old_frame = match self.cow_source_frame(page_table, vpn) {
        Ok(frame) => frame,
        Err(err) => {
            warn!(
                "[copy_on_write] mapped COW page has no resident frame: vpn={:?}, state={}, area={:?}",
                vpn,
                self.inner.frame_state_name(&vpn),
                self
            );
            return Err(err);
        }
    };
    // cow_source_frame() returns a cloned Arc, so a page owned only by this
    // VMA has two strong refs here: the VMA entry and this local handle.
    if Arc::strong_count(&old_frame) <= 2 {
        let old_ppn = old_frame.ppn;
        UserMapper::new(page_table).set_user_flags(vpn, self.map_perm)?;
        Ok(old_ppn)
    } else {
        // do copy in this case
        let old_ppn = old_frame.ppn;
        if !UserMapper::new(page_table).is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        // alloc new frame
        let new_frame = unsafe { frame_alloc_uninit().ok_or(MemoryError::OutOfMemory)? };
        let new_ppn = new_frame.ppn;
        // copy data
        new_ppn
            .get_bytes_array()
            .copy_from_slice(old_ppn.get_bytes_array());
        let old_frame = self
            .inner
            .remove_in_memory(&vpn)
            .ok_or(MemoryError::BadAddress)?;
        if let Err(err) = self.inner.alloc_in_memory(vpn, new_frame) {
            let _ = self.inner.alloc_in_memory(vpn, old_frame);
            return Err(err);
        }
        if UserMapper::new(page_table).set_ppn(vpn, new_ppn).is_err() {
            if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                drop(new_frame);
            }
            let _ = self.inner.alloc_in_memory(vpn, old_frame);
            return Err(MemoryError::NotMapped);
        }
        if UserMapper::new(page_table)
            .set_user_flags(vpn, self.map_perm)
            .is_err()
        {
            let _ = UserMapper::new(page_table).set_ppn(vpn, old_ppn);
            if let Some(new_frame) = self.inner.remove_in_memory(&vpn) {
                drop(new_frame);
            }
            let _ = self.inner.alloc_in_memory(vpn, old_frame);
            return Err(MemoryError::NotMapped);
        }
        Ok(new_ppn)
    }
}
```

这个分支是 CoW 的性能关键：如果旧 frame 已经只被当前 VMA 持有，写 fault 不需要复制 4 KiB 数据，只要恢复页表权限即可。只有 `Arc::strong_count()` 显示还有其他 VMA/页表路径共享同一个 frame 时，才分配新 frame 并复制旧内容。这样 fork 后父子进程只读大量页面时几乎不付复制成本，而真正写入的页面才被拆开。

读 COW bug 时要同时核对三件事：

| 状态 | 正确关系 |
|------|----------|
| 父/子 VMA 的 frame `Arc` | fork 后 private resident 页应共享同一个 frame。 |
| 父/子 PTE 权限 | private writable 页 fork 后应撤销 W，shared 映射不走这条规则。 |
| 写 fault 后的 PTE PPN | 复制路径应指向新 frame；独占恢复路径应保留旧 PPN 但恢复 W。 |

## 10. COW 回滚

复制路径包含多个可失败步骤。实现会在失败时恢复旧状态：

| 失败点 | 回滚 |
|--------|------|
| 新 frame 记录到 `VmPageStore` 失败 | 重新插入旧 frame |
| `set_ppn()` 失败 | 删除新 frame，恢复旧 frame |
| `set_user_flags()` 失败 | 尝试把 PTE PPN 改回旧页，恢复旧 frame |

这避免 VMA resident frame 和 PTE 指向分离。

## 11. shared 映射不走 COW

`MAP_SHARED` 映射不撤销父进程 W 作为 COW。文件 shared writable 的读缺页会清 W，是为了确保首次 store 标脏 page cache，不是为了复制私有页。

shared anonymous writable 在 mmap 时预分配 shared frames：

1. fork clone `Arc<FrameTracker>`。
2. 子进程 VMA 继承相同 resident frames。
3. 首次访问按 `ResidentWithoutPte` 映射同一 PPN。
4. 写入对父子可见。

## 12. 文件 private 映射

文件 private writable 的语义分两段：

| 场景 | 行为 |
|------|------|
| 首次读 | `filemap_read_fault()` 映射 page cache frame，PTE 去 W |
| 首次写 | 若 PTE 已映射，走 COW；若未映射且 store 先到达，走 `filemap_private_fault()` 分配私有页 |

两种写路径最终都会得到 private frame，不修改 page cache。

## 13. wipe_on_fork

`wipe_on_fork` 使用 `Vma::from_another(area)`：

```rust
/// Copier, but the physical pages are not allocated,
/// thus leaving `data_frames` empty.
pub fn from_another(another: &Vma) -> Self {
    Self {
        inner: VmPageStore::new(VPNRange::new(
            another.inner.vpn_range.get_start(),
            another.inner.vpn_range.get_end(),
        )),
        map_perm: another.map_perm,
        map_file: another.map_file.clone(),
        map_file_offset: another.map_file_offset,
        may_write: another.may_write,
        write_sealed: another.write_sealed,
        flags: another.flags,
        wipe_on_fork: another.wipe_on_fork,
        dont_fork: another.dont_fork,
        fork_inherited: another.fork_inherited,
    }
}
```

它保留范围、权限、文件信息和 flags，但 `VmPageStore` 为空。子进程访问时重新 lazy alloc 或从文件读取。

## 14. trap context 复制

fork 复制用户 VMA 后，还要复制当前 task 的 trap context。实现不会猜测 `last_non_user()`，而是根据 `trap_cx_slot` 精确计算 trap context VPN：

```rust
let trap_cx_vpn = VirtAddr::from(trap_cx_bottom_from_slot(trap_cx_slot)).into();
```

这是为了避免 clone/exit 后存在 stale 或更高编号非用户 VMA 时复制错误的 trap context。

## 15. 与 exec 的区别

fork 通过 `from_existing_user()` 复制地址空间；exec 通过 `from_elf()` 创建全新地址空间。

| 操作 | 地址空间 |
|------|----------|
| fork/clone 非共享 VM | 复制 VMA，使用 COW |
| clone 共享 VM | 进程/线程层共享同一 `AddressSpace` |
| execve | 丢弃旧地址空间，按新 ELF 重建 |

`CLONE_VM` 的线程语义由 process/task 层决定，不在 COW 路径中复制页表。

## 16. 调试核对点

| 现象 | 检查 |
|------|------|
| fork 后父子互相覆盖 private 数据 | 父/子 PTE W 是否撤销，TLB 是否 flush |
| fork 后 shared anonymous 不共享 | `do_mmap()` 预分配 frame 和 `VmPageStore::try_clone()` |
| `MADV_DONTFORK` 仍继承 | `from_existing_user()` 是否过滤 `dont_fork` |
| `MADV_WIPEONFORK` 子进程仍有旧内容 | 是否走 `Vma::from_another()` |
| COW 后 PTE 指向新页但 VMA 仍旧页 | `copy_on_write()` 回滚和 `set_ppn()` 顺序 |
