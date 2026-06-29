---
title: "mmap、munmap、mprotect 与 brk"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, mmap, brk, mprotect, mincore]
---

# mmap、munmap、mprotect 与 brk

## 1. 调用链

用户态内存映射 syscall 首先进入 `os/src/syscall/process/mm.rs`，完成参数解析和 Linux 兼容性校验；随后进入 `os/src/mm/mmap.rs` 操作 `AddressSpace`。

```
sys_mmap / sys_munmap / sys_mprotect / sys_brk
  └── syscall/process/mm.rs
        ├── parse_mmap_prot()
        ├── parse_mmap_flags()
        ├── 文件描述符与权限校验
        └── process.vm().lock()
              └── mm/mmap.rs
                    ├── do_mmap()
                    ├── do_munmap()
                    ├── do_mprotect()
                    └── do_sbrk()
```

syscall 层负责把用户 ABI 转换成 `MapPermission`、`MapFlags`、`Arc<dyn IndexNode>` 等内核对象；MM 层负责地址选择、VMA 插入/删除、页表更新和元数据维护。

## 2. mmap 参数解析

`parse_mmap_prot()` 将 POSIX `PROT_*` 转成 `MapPermission`：

| 用户标志 | 内核权限 |
|----------|----------|
| `PROT_READ` | `R` |
| `PROT_WRITE` | `W` |
| `PROT_EXEC` | `X` |
| 任意用户映射 | `U` |

`parse_mmap_flags()` 将 `MAP_*` 转成 `MapFlags`。当前 `MapFlags` 包含：

| 类别 | 标志 |
|------|------|
| 映射类型 | `MAP_SHARED`, `MAP_PRIVATE`, `MAP_SHARED_VALIDATE` |
| 文件/匿名 | `MAP_ANONYMOUS` |
| 地址控制 | `MAP_FIXED`, `MAP_FIXED_NOREPLACE`, `MAP_GROWSDOWN` |
| 行为标记 | `MAP_DENYWRITE`, `MAP_EXECUTABLE`, `MAP_LOCKED`, `MAP_POPULATE`, `MAP_STACK`, `MAP_NONBLOCK` |
| hugetlb 兼容 | `MAP_HUGETLB`, `MAP_HUGE_2MB`, `MAP_HUGE_1GB` |

syscall 层会拒绝不兼容组合。例如 `MAP_SHARED_VALIDATE` 携带未知 flag 时返回 `EOPNOTSUPP`。

## 3. 文件映射校验

非匿名映射必须提供合法 fd。`sys_mmap()` 明确保证坏 fd 的 `EBADF` 优先级：

```text
if !MAP_ANONYMOUS and fd invalid
  -> EBADF
```

文件映射还会检查：

| 条件 | 错误 |
|------|------|
| 非匿名映射 fd 无效 | `EBADF` |
| offset 非页对齐 | `EINVAL` |
| 文件不可读但映射需要读/执行 | `EACCES` |
| shared writable 但 fd 不可写 | `EACCES` |
| 文件类型不支持映射 | `EACCES` |
| memfd 写 seal 冲突 | `EPERM` |

`/dev/zero` 按匿名映射语义处理。

## 4. do_mmap 主流程

`do_mmap()` 的核心步骤：

```text
do_mmap(start, len, prot, flags, offset, map_file)
  ├── start 必须页对齐
  ├── checked_user_range(start, len)
  ├── overcommit 检查
  ├── fixed 地址处理
  │     ├── MAP_FIXED_NOREPLACE 冲突 -> EEXIST
  │     └── MAP_FIXED 覆盖旧范围并清 locked pages
  ├── 非 fixed 地址处理
  │     ├── hint 可用则使用 hint
  │     └── 否则 find_free_mmap_range()
  ├── 尝试合并 lazy private anonymous VMA
  ├── 创建 Vma
  ├── 非匿名映射绑定 map_file 与 offset
  ├── writable anonymous shared 预分配帧
  ├── VmaSet::insert_vma()
  └── MAP_LOCKED 标记 locked pages
```

返回值为映射起始虚拟地址；失败返回负 errno。

`do_mmap()` 只负责建立“虚拟地址范围的承诺”，并不等价于立刻把每一页都映射到物理页。普通匿名私有映射会创建 VMA 和 `VmPageStore::Unallocated` 状态，第一次访问时再由缺页路径分配清零页；文件映射会记录 inode 和 file offset，第一次访问时再通过 `filemap.rs` 找 PageCache 页。这个设计让大映射可以低成本创建，也让 `fork` 后的 CoW 可以只处理实际 resident 的页面。

读 `do_mmap()` 时要同时看三个状态是否一致：

| 状态 | 作用 |
|------|------|
| `VmaSet` 中的 VMA | 决定地址范围、权限、shared/private、文件后端和 fork 行为。 |
| `mmap_holes` | 记录哪些虚拟地址洞可用于下一次非 fixed mmap。 |
| 页表 PTE | 对 lazy 映射通常暂时不存在，只有 eager/shared/locked 或 fault 后才出现。 |

如果 bug 表现为“mmap 返回地址错误”，优先看地址选择和 hole 管理；如果表现为“访问后 fault 错误”，优先看 VMA 权限、`VmPageStore` 和 `page_fault.rs` 分类。

## 5. 地址选择规则

fixed 与非 fixed 的差异：

| 模式 | 行为 |
|------|------|
| `MAP_FIXED_NOREPLACE` | 如果目标范围与已有 VMA 重叠，返回 `EEXIST` |
| `MAP_FIXED` | 先 `unmap_range(..., allow_empty = true)` 覆盖目标范围 |
| 非 fixed 且 hint 可用 | 使用 hint |
| 非 fixed 且 hint 不可用 | 从 `mmap_holes` 找第一个满足长度和对齐的洞 |

`MAP_FIXED` 成功覆盖后会调用 `set_locked_pages(start_vpn, end_vpn, false)`，避免旧映射的 locked 标记泄漏到新映射。

## 6. lazy private anonymous 合并

非 `MAP_LOCKED` 的匿名私有映射可能与前一个 VMA 合并：

```text
try_merge_lazy_private_mmap(start_va, len, prot, flags)
  ├── 新范围必须空闲
  ├── 前一段 end == 新 start
  ├── 前一段必须 vm_can_merge_lazy_private()
  ├── reserve mmap range
  └── expand_to(new_end)
```

合并条件要求两段都是 `MAP_PRIVATE | MAP_ANONYMOUS`，权限和 fork 继承约束兼容。fork 继承来的匿名 VMA 通过 `fork_inherited` 避免和后续 child-only mmap 错误合并。

## 7. writable anonymous MAP_SHARED

匿名 shared 且可写时，`do_mmap()` 会预分配 shared frames，但不立即安装用户 PTE：

```rust
if flags.contains(MapFlags::MAP_SHARED)
    && new_area.map_file.is_none()
    && new_area.map_perm.contains(MapPermission::W)
{
    if len > MAX_EAGER_MMAP_SIZE {
        return ENOMEM;
    }
    for vpn in vpn_range {
        new_area.alloc_one_zeroed_unmapped(vpn)?;
    }
}
```

`MAX_EAGER_MMAP_SIZE = 1 GiB`。这样 fork 后父子继承同一批 shared frame，同时 mincore/mlock2 等仍能观察到“PTE 尚未安装”的懒映射状态。

## 8. overcommit

`charges_overcommit()` 只对匿名可写映射计入承诺：

```rust
flags.contains(MapFlags::MAP_ANONYMOUS) && prot.contains(MapPermission::W)
```

`overcommit_allows(current, additional)` 来自 `os/src/mm/sysctl.rs`：

| `overcommit_memory` | 语义 |
|---------------------|------|
| `0` | additional 不超过 reported memory |
| `1` | 总是允许 |
| `2` | current + additional 不超过 commit limit |

commit limit 使用 reported memory 与 ratio 计算，并被 `64 MiB` 上限截断。

## 9. do_sbrk

`do_sbrk()` 实现 program break 增减：

1. `old_pt = heap_pt`。
2. `limit = heap_bottom + USER_HEAP_SIZE`。
3. 根据 increment 计算 `new_pt`，检查上下界和溢出。
4. 把 old/new break 向上取整到页边界。
5. 增长时用固定匿名私有 `do_mmap()` 映射新增页。
6. 收缩时用 `do_munmap()` 删除多余页。
7. 成功后更新 `heap_pt = new_pt`。

增长前还会检查目标范围是否与非 heap 兼容 VMA 冲突。`brk_overlap_blocks()` 只允许匿名私有可读写用户 VMA 作为 heap 兼容区域。

`do_sbrk()` 的完整实现如下：

```rust
pub(super) fn do_sbrk<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    increment: isize,
) -> usize {
    let old_pt = address_space.heap_pt;
    let heap_bottom = address_space.heap_bottom;
    let Some(limit) = heap_bottom.checked_add(USER_HEAP_SIZE) else {
        warn!(
            "[sbrk] heap limit overflow! heap_bottom: {:X}, heap_size: {:X}",
            heap_bottom, USER_HEAP_SIZE
        );
        return old_pt;
    };
    let new_pt = if increment > 0 {
        match old_pt.checked_add(increment as usize) {
            Some(new_pt) => new_pt,
            None => {
                warn!(
                    "[sbrk] grow overflow! old_pt: {:X}, increment: {:X}",
                    old_pt, increment
                );
                return old_pt;
            }
        }
    } else if increment < 0 {
        let Some(delta) = increment.checked_neg().map(|delta| delta as usize) else {
            warn!(
                "[sbrk] shrink overflow! old_pt: {:X}, increment: {:X}",
                old_pt, increment
            );
            return old_pt;
        };
        match old_pt.checked_sub(delta) {
            Some(new_pt) => new_pt,
            None => {
                warn!(
                    "[sbrk] shrink underflow! old_pt: {:X}, decrement: {:X}",
                    old_pt, delta
                );
                return old_pt;
            }
        }
    } else {
        return old_pt;
    };

    if new_pt < heap_bottom {
        warn!(
            "[sbrk] out of the lowerbound! lowerbound: {:X}, old_pt: {:X}, new_pt: {:X}",
            heap_bottom, old_pt, new_pt
        );
        return old_pt;
    }
    if new_pt > limit {
        warn!(
            "[sbrk] out of the upperbound! upperbound: {:X}, old_pt: {:X}, new_pt: {:X}",
            limit, old_pt, new_pt
        );
        return old_pt;
    }

    let Some(old_page_end) = page_round_up_addr(old_pt) else {
        warn!("[sbrk] old break round-up overflow! old_pt: {:X}", old_pt);
        return old_pt;
    };
    let Some(new_page_end) = page_round_up_addr(new_pt) else {
        warn!("[sbrk] new break round-up overflow! new_pt: {:X}", new_pt);
        return old_pt;
    };

    if new_pt > old_pt {
        if new_page_end > old_page_end {
            let len = new_page_end - old_page_end;
            let start_vpn = VirtAddr::from(old_page_end).floor();
            let end_vpn = VirtAddr::from(new_page_end).ceil();
            if address_space
                .vmas
                .iter()
                .any(|area| area.vm_overlaps(start_vpn, end_vpn) && brk_overlap_blocks(area))
            {
                return old_pt;
            }
            if !crate::mm::overcommit_allows(address_space.committed_bytes(), len) {
                return old_pt;
            }
            let ret = do_mmap(
                address_space,
                old_page_end,
                len,
                MapPermission::R | MapPermission::W | MapPermission::U,
                MapFlags::MAP_ANONYMOUS | MapFlags::MAP_FIXED | MapFlags::MAP_PRIVATE,
                0,
                None,
                true,
                false,
            );
            if ret < 0 {
                warn!(
                    "[sbrk] heap grow mmap failed: start={:X}, len={:X}, err={}",
                    old_page_end, len, ret
                );
                return old_pt;
            }
        }
    } else if old_page_end > new_page_end {
        let len = old_page_end - new_page_end;
        if let Err(err) = do_munmap(address_space, new_page_end, len) {
            warn!(
                "[sbrk] heap shrink munmap failed: start={:X}, len={:X}, err={}",
                new_page_end, len, err
            );
            return old_pt;
        }
    }

    address_space.heap_pt = new_pt;
    new_pt
}
```

这个函数的失败语义不是返回负 errno，而是保持并返回旧 break。`sys_brk()` 会把“返回 break 值”的 Linux 语义暴露给用户；因此 `do_sbrk()` 中的越界、overcommit、VMA 冲突和底层 mmap/munmap 失败都会回退为 `old_pt`。

## 10. do_munmap

`do_munmap()` 要求：

| 条件 | 错误 |
|------|------|
| len 为 0 | `EINVAL` |
| 地址范围溢出或超过 `USER_VA_END` | `EINVAL` |
| start 非页对齐 | `EINVAL` |

主逻辑调用：

```rust
address_space.vmas.unmap_range(
    &mut address_space.page_table,
    start_vpn,
    end_vpn,
    true,
)
```

`allow_empty = true` 意味着 munmap 空洞不是错误。实际删除时，`VmaSet` 会分裂覆盖范围、unmap resident pages、释放 mmap hole 并维护统计。

`do_munmap()` 本身只做范围合法性和页对齐检查，实际复杂度在 `VmaSet::unmap_range()`：

```rust
pub(super) fn do_munmap<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    start: usize,
    len: usize,
) -> Result<(), isize> {
    let (start_va, end_va) = checked_user_range(start, len)?;
    if !start_va.aligned() {
        warn!("[munmap] Not aligned");
        return Err(EINVAL);
    }
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    address_space
        .vmas
        .unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)
        .map(|_| ())
}
```

由于 `checked_user_range()` 对 `len == 0` 返回 `EINVAL`，`munmap(addr, 0)` 不会进入 VMA 删除逻辑。`start_va.aligned()` 要求用户传入起始地址页对齐；结束地址通过 `ceil()` 覆盖最后一页的部分范围。

## 11. do_mprotect

`do_mprotect()` 要求 start 页对齐，len 为 0 时直接成功。范围合法后进入 `VmaSet::protect_range()`：

1. 第一遍扫描所有覆盖 VMA，检查范围存在。
2. 如果新权限含 W 且 VMA 是 shared：
   - `write_sealed` 返回 `EPERM`。
   - `may_write == false` 返回 `EACCES`。
3. 第二遍对每个覆盖片段执行 VMA 分裂。
4. `protect_area()` 更新 resident pages 的 PTE flags。
5. 更新 `area.map_perm`。

私有映射的 resident pages 实际 PTE 权限会去掉 W：

```rust
let actual_prot = if area.flags.contains(MapFlags::MAP_SHARED) {
    prot
} else {
    prot - MapPermission::W
};
```

这保留私有可写映射的 COW 行为：VMA 允许 W，但 PTE 首次写仍要 fault。

`do_mprotect()` 的入口实现如下：

```rust
pub(super) fn do_mprotect<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    addr: usize,
    len: usize,
    prot: MapPermission,
) -> Result<(), isize> {
    if len == 0 {
        return Ok(());
    }
    let (start_va, end_va) = checked_user_range(addr, len)?;
    // addr is not a multiple of the system page size.
    if !start_va.aligned() {
        warn!("[mprotect] Not aligned");
        return Err(EINVAL);
    }
    warn!(
        "[mprotect] addr: {:X}, len: {:X}, prot: {:?}",
        addr, len, prot
    );
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    address_space
        .vmas
        .protect_range(&mut address_space.page_table, start_vpn, end_vpn, prot)
}
```

`len == 0` 直接成功，这是 `mprotect` 与 `munmap` 的重要差异。非零长度时同样要求起始地址页对齐，并把范围交给 `protect_range()` 做 VMA 分裂、权限校验和 resident PTE 更新。

## 12. madvise 与 mincore

`AddressSpace::madvise()` 支持的分支位于 `VmaSet::advise_range()`：

| advice | 行为 |
|--------|------|
| `MADV_DONTNEED` | 对匿名私有 VMA 丢弃 resident pages |
| `MADV_FREE` | 要求 anonymous private，否则 `EINVAL` |
| `MADV_DONTFORK` | 分裂范围并设置 `dont_fork` |
| `MADV_DOFORK` | 分裂范围并清除 `dont_fork` |
| `MADV_WIPEONFORK` | 要求 anonymous private，设置 `wipe_on_fork` |
| `MADV_KEEPONFORK` | 清除 `wipe_on_fork` |
| `MADV_MERGEABLE/UNMERGEABLE` | 要求 VMA 可写 |

`mincore_range()` 判断 resident 的条件是：

1. 页表中已有有效映射；或
2. 文件映射对应 page cache 已包含该页。

这与文件 mmap 的懒 PTE 安装配合：页可能不在 PTE 中，但已在 page cache 中。

## 13. mlock 相关映射标记

`MAP_LOCKED` 在 `do_mmap()` 插入 VMA 后只标记 `locked_pages`，不逐页 fault-in。`mlock()` 才会逐页调用 `fault_in_user_va(..., FaultAccess::Load)`。

区别：

| 操作 | 是否立即 fault-in | 是否标记 locked |
|------|-------------------|------------------|
| `MAP_LOCKED` | 否 | 是 |
| `mlock` | 是 | 是 |
| `mlock2(MLOCK_ONFAULT)` | 否 | 是 |
| `munlock` | 否 | 清除 |

## 14. 错误码边界

| 场景 | 错误 |
|------|------|
| `mmap` start 非页对齐 | `EINVAL` |
| `mmap` len 为 0 | `EINVAL` |
| `MAP_FIXED_NOREPLACE` 重叠 | `EEXIST` |
| 非匿名映射 offset 非页对齐 | `EINVAL` |
| 非匿名映射缺少文件对象 | `EBADF` |
| writable anonymous shared 超过 1 GiB | `ENOMEM` |
| overcommit 不允许 | `ENOMEM` |
| `mprotect` shared 写权限被 seal 禁止 | `EPERM` |
| `mprotect` shared 写权限但文件不可写 | `EACCES` |
| `munmap` 空洞 | 成功 |

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| `mmap` hint 没有生效 | hint 范围是否完全空闲 |
| `MAP_FIXED` 后旧 locked 状态残留 | `set_locked_pages(..., false)` 是否执行 |
| 私有映射写入未触发 COW | `mprotect/protect_area` 是否错误保留 PTE W |
| shared anonymous fork 后不共享 | `do_mmap()` 是否预分配 `VmPageStore` frame |
| mincore 对文件映射返回不符 | page cache residency 与 PTE residency 要分开看 |
