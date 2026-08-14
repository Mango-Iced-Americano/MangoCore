---
title: "地址空间、VMA 与用户映射"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-14
tags: [mm, address-space, vma, elf, maps, mmu-gather, membarrier]
---

# 地址空间、VMA 与用户映射

## 1. 总体结构

进程地址空间在同一文件中分成外层共享对象和锁内数据：

```rust
pub struct AddressSpace<T: PageTable> {
    inner: Mutex<AddressSpaceInner<T>>,
    tlb: TlbContext,
    private_expedited_registered: AtomicBool,
}

pub struct AddressSpaceInner<T: PageTable> {
    page_table: T,
    vmas: VmaSet,
    heap_bottom: usize,
    heap_pt: usize,
    locked_pages: BTreeSet<VirtPageNum>,
    mmu_gather: MmuGather,
}
```

字段职责：

| 字段 | 说明 |
|------|------|
| `AddressSpace.inner` | 串行化同一 MM 的 VMA、PTE 和 CPU 激活登记 |
| `AddressSpace.tlb` | 与共享 MM 同寿命的 ID、active CPU mask、generation 与 per-CPU observed |
| `AddressSpace.private_expedited_registered` | MM-owned PRIVATE_EXPEDITED 注册状态 |
| `AddressSpaceInner.page_table` | 当前进程页表 |
| `AddressSpaceInner.vmas` | 用户与少量进程私有内核映射的 VMA 集合 |
| `heap_bottom/heap_pt` | ELF 加载后的 heap 起点与当前 program break |
| `locked_pages` | `mlock/mlock2/mlockall` 标记的页 |
| `mmu_gather` | 当前一次 `write()` 内的失效范围和待退休 frame；解锁时移交给 `TlbFlush` |

`AddressSpace<T>` 不直接保存文件描述符、进程 ID 或信号状态。这些属于 `ProcessControlBlock`。
PCB 以 `Arc<AddressSpace<T>>` 持有 VM；读操作经 `read()`，可能改 PTE 的操作经
`write()/try_write()`。后者在内部释放 VM 锁后才等待 user-TLB ack，调用方无法取得
可变 guard 并把锁带过远端等待点。

新页的内容初始化遵循“先填充、后发布 PTE”：`Vma::alloc_one_zeroed_unmapped()`
先让 VMA 拥有 frame 但不建立用户映射，构造路径在这个未发布窗口填充数据，
再用 `map_existing_in_memory()` 安装 PTE。初始化或建映失败时，
`remove_unmapped_frame()` 只回滚尚无 PTE 的 frame；一旦 PTE 已可见，必须走正式
unmap + TLB retire 协议。这个顺序防止同一 MM 的其它 CPU 在 filemap/ELF 拷贝完成
前观察到半成品页，也使未发布 frame 的独占写入条件可以明确证明。

### 1.1 方法地图

| 方法 | 源码位置 | 作用 |
|------|----------|------|
| `AddressSpace::new()` | `address_space.rs` | 将尚未发布的锁内数据包装为共享 VM，并建立独立 `TlbContext`。 |
| `AddressSpace::read()` | `address_space.rs` | 在 VM 锁内提供不可变访问，不进入 TLB 修改协议。 |
| `AddressSpace::write()/try_write()` | `address_space.rs` | 锁内记录修改、`seal()`，解锁后执行 `TlbFlush`。 |
| `AddressSpace::activate_on()` | `address_space.rs` | 在 VM 锁内登记当前 CPU、完成首次 membarrier fence、追平 generation，并取得页表 token。 |
| `AddressSpace::deactivate_on()` | `address_space.rs` | 在 idle 栈上以完整屏障切离当前 CPU，并在 VM 锁内清除 active bit。 |
| `register_private_expedited()` | `address_space.rs` | 为共享 MM 注册 PRIVATE_EXPEDITED。 |
| `private_expedited_targets()` | `address_space.rs` | 在 VM 锁内冻结当前 active CPU mask；未注册时返回 `None`。 |
| `AddressSpaceInner::new_bare()` | `address_space.rs` | 创建空的页表/VMA 数据。 |
| `AddressSpaceInner::token()` | `address_space.rs` | 返回页表 token。 |
| `AddressSpaceInner::from_elf()` | `address_space.rs` | 从 ELF 构造尚未发布的用户地址空间。 |
| `AddressSpaceInner::from_elf_inode()` | `address_space.rs` | 从 inode 构造 PT_LOAD 按需装页的未发布地址空间。 |
| `AddressSpaceInner::from_existing_user()` | `address_space.rs` | fork/非 `CLONE_VM` clone 复制地址空间，配合 VMA CoW。 |
| `do_page_fault()` | `address_space.rs:631` | trap/uaccess 共用缺页入口。 |
| `fault_in_user_va()` | `address_space.rs:659` | syscall 用户指针入口：先检查已映射的 `U+R/W` PTE，未命中才 fault-in 并做权限复核。 |
| `mapped_user_va()` | `address_space.rs` | 无副作用地验证当前 PTE 与 RAM 物理地址；只供 `fault_in_user_va()` 快路径使用。 |
| `sbrk()` | `address_space.rs:1090` | 转入 `mmap.rs::do_sbrk()` 调整 program break。 |
| `mmap()` | `address_space.rs:1094` | 转入 `mmap.rs::do_mmap()` 创建映射。 |
| `munmap()` | `address_space.rs:1130` | 释放映射范围，并同步清理 locked page 标记。 |
| `mprotect()` | `address_space.rs:1142` | 修改 VMA/PTE 权限。 |

阅读地址空间代码时，先区分入口来自哪里：`execve` 和 initproc 优先走 `from_elf_inode()`，仅在文件系统没有 PageCache 时回退 `from_elf()`；fork 走 `from_existing_user()`，用户触页走 `do_page_fault()`，syscall 用户指针走 `fault_in_user_va()`，显式内存 syscall 走 `mmap()/munmap()/mprotect()/sbrk()`。

### 1.2 CPU 驻留与 generation

`activate_on()`/`deactivate_on()` 只允许由目标 CPU 自己调用，并与 PTE writer 共用
`AddressSpace.inner`。每 CPU 的调度状态保存精确的旧 `AddressSpace` Arc：trap-return
切换 MM 时先从旧 MM leave，再进入新 MM；任务真正切回 idle 栈时也会 leave。这个 Arc
不能改成重新读取 `process.vm()`，因为 exec 可能已经把 PCB 中的指针替换为新 MM。

writer 在锁内冻结 active mask。mask 为空只表示当前没有 CPU 可直接返回该 MM，不表示
硬件里绝无旧 ASID 翻译，因此 PTE 修改仍推进 generation；下次 `activate_on()` 发现
observed 落后时先做本地全用户失效，再取得页表根。零目标提交释放退休 frame 时，即使
另一个 CPU 已经并发开始进入，它也会在使用页表根前先观察新 generation 并完成补刷。

`from_existing_user()` 是 fork/非 `CLONE_VM` clone 复制地址空间的核心函数：

```rust
pub fn from_existing_user(
    user_space: &mut AddressSpaceInner<T>,
    trap_cx_slot: usize,
    trap_cx: &TrapContext,
) -> Result<AddressSpaceInner<T>, isize> {
    let mut address_space = Self::new_bare();
    if should_map_trampoline!() {
        address_space.map_trampoline();
    }
    address_space.map_signaltrampoline();
    address_space.heap_bottom = user_space.heap_bottom;
    address_space.heap_pt = user_space.heap_pt;
    if address_space
        .vmas
        .try_reserve(user_space.vmas.len())
        .is_err()
    {
        return Err(crate::syscall::errno::ENOMEM);
    }
    for area in user_space
        .vmas
        .iter()
        .filter(|area| area.vm_is_user() && !area.dont_fork)
    {
        let mut new_area = if area.wipe_on_fork {
            Vma::from_another(area)
        } else {
            let mut cloned = area.try_clone()?;
            cloned
                .map_from_existing_page_table(
                    &mut address_space.page_table,
                    &mut user_space.page_table,
                )
                .map_err(|_| crate::syscall::errno::ENOMEM)?;
            cloned
        };
        new_area.mark_fork_inherited();
        address_space.vmas.push(new_area)?;
    }
    let trap_cx_vpn: VirtPageNum =
        VirtAddr::from(trap_cx_bottom_from_slot(trap_cx_slot)).into();
    let trap_cx_area = user_space
        .vmas
        .get_by_start(trap_cx_vpn)
        .filter(|area| !area.vm_is_user())
        .ok_or(crate::syscall::errno::EINVAL)?;
    let area = Vma::from_another(trap_cx_area);
    let trap_cx_data = unsafe {
        core::slice::from_raw_parts(
            (trap_cx as *const TrapContext).cast::<u8>(),
            core::mem::size_of::<TrapContext>(),
        )
    };
    address_space
        .push(area, Some(trap_cx_data))
        .map_err(|_| crate::syscall::errno::ENOMEM)?;

    Ok(address_space)
}
```

该函数只复制用户 VMA，跳过 `dont_fork`，对 `wipe_on_fork` 只复制元数据不复制页内容。当前线程的 trap context 单独根据 `trap_cx_slot` 定位，不依赖最后一个非用户 VMA，避免 clone/exit 之后 slot 号不连续导致复制错误。

### 1.2 uaccess 已映射页快路径

syscall 的 `fault_in_user_va()` 不会为已满足访问权限的 PTE 重新进入缺页处理。其 `mapped_user_va()` 检查用户位、按 Load/Store 区分的 `R/W` 位和可分配 RAM 物理地址；任一条件不成立时仍交给既有缺页、COW 或 VMA 扩展路径。快路径不写 PTE，因此不改变现有 TLB invalidate 约束。

## 2. VMA 数据结构

单段映射由 `os/src/mm/vma.rs:34` 的 `Vma` 表示：

```rust
pub struct Vma {
    pub inner: VmPageStore,
    pub map_perm: MapPermission,
    pub map_file: Option<Arc<dyn IndexNode>>,
    pub(super) elf_lazy: Option<Arc<ElfLazyBacking>>,
    pub map_file_offset: usize,
    pub may_write: bool,
    pub write_sealed: bool,
    pub flags: MapFlags,
    pub wipe_on_fork: bool,
    pub dont_fork: bool,
    pub fork_inherited: bool,
}
```

字段语义：

| 字段 | 说明 |
|------|------|
| `inner` | VPN 范围和每页状态 |
| `map_perm` | VMA 对外权限，包含 `R/W/X/U` |
| `map_file` | 文件映射后端，匿名映射为 `None` |
| `elf_lazy` | ELF PT_LOAD 按需页的不可变后备配方，普通 mmap 为 `None` |
| `map_file_offset` | VMA 起点对应文件偏移 |
| `may_write` | 文件映射是否允许获得写权限 |
| `write_sealed` | memfd seal 对写映射的限制 |
| `flags` | mmap 标志，如 shared/private/anonymous/locked/growsdown |
| `wipe_on_fork` | fork 后子进程清空该 VMA |
| `dont_fork` | fork 时不继承该 VMA |
| `fork_inherited` | fork 继承来的匿名 VMA，限制后续合并 |

`Vma` 的字段可以理解成三类状态：

| 类别 | 字段 | 说明 |
|------|------|------|
| 范围和页状态 | `inner` | `VmPageStore` 保存 VPN 范围以及每页是 unallocated、in memory、compressed 还是 swapped。 |
| 权限和来源 | `map_perm`, `map_file`, `elf_lazy`, `map_file_offset`, `may_write`, `write_sealed` | 决定 fault 能否成功，以及文件或 ELF 映射从哪里取数据。 |
| fork/mmap 行为 | `flags`, `wipe_on_fork`, `dont_fork`, `fork_inherited` | 决定 shared/private、anonymous/file、growdown、locked 和 fork 继承规则。 |

`Vma::try_new()` 位于 `vma.rs:82`，只创建范围和元数据；除特殊路径外，它不为每一页分配 frame。`Vma::try_clone()` 位于 `vma.rs:57`，会克隆 VMA 元数据和页状态，用于 fork 前的地址空间复制准备。

写缺页进入 COW 时，实际复制由 `Vma::copy_on_write()` 完成：

```rust
pub fn copy_on_write<T: PageTable>(
    &mut self,
    mapper: &mut UserMapper<'_, T>,
    vpn: VirtPageNum,
) -> Result<PhysPageNum, MemoryError> {
    let old_frame = self.cow_source_frame(mapper, vpn)?;
    if Arc::strong_count(&old_frame) <= 2 {
        let old_ppn = old_frame.ppn;
        mapper.set_user_flags(vpn, self.map_perm)?;
        Ok(old_ppn)
    } else {
        // 省略 frame store 与 PTE 的失败回滚。
        let new_frame = unsafe { frame_alloc_uninit().ok_or(MemoryError::OutOfMemory)? };
        let new_ppn = new_frame.ppn;
        mapper.set_ppn(vpn, new_ppn)?;
        mapper.set_user_flags(vpn, self.map_perm)?;
        mapper.retire_frame(old_frame);
        Ok(new_ppn)
    }
}
```

`Arc::strong_count(&old_frame) <= 2` 时，说明除 VMA 内部引用和本地临时引用外没有其他共享者，可以直接恢复写权限；否则分配新 frame、复制旧页数据、替换 `VmPageStore` 和页表 PPN。示例省略了源码中的失败回滚；成功替换的旧 frame，以及曾短暂出现在 PTE 中的回滚新 frame，都由同一个 `UserMapper` 交给 `MmuGather`，延迟到 TLB 同步完成后释放。

## 3. VmaSet 管理模型

`os/src/mm/vma_set.rs:15` 的 `VmaSet` 使用两个 `BTreeMap`：

```rust
pub(super) struct VmaSet {
    vmas: BTreeMap<VirtPageNum, Vma>,
    mmap_holes: BTreeMap<VirtPageNum, VirtPageNum>,
    user_area_count: usize,
    user_page_count: usize,
}
```

| 字段 | 作用 |
|------|------|
| `vmas` | 以起始 VPN 为 key 的 VMA 表 |
| `mmap_holes` | mmap 可用洞表，用于自动选址 |
| `user_area_count` | 用户 VMA 数量，用于 `max_map_count` |
| `user_page_count` | 用户映射页数，用于统计 |

`mmap_holes` 的初始范围由架构决定：

| 架构 | 范围 |
|------|------|
| rv64 | `MMAP_BASE..MMAP_END` |
| la64 | `USR_MMAP_BASE..USR_MMAP_END` |

这解释了为什么 `mmap` 自动选址不扫描整个用户地址空间，而是在专用 mmap 窗口内找洞。

`VmaSet::with_capacity()` 位于 `vma_set.rs:85`，初始化时把架构 mmap 区间整体放入 `mmap_holes`。之后每次 `insert_vma()`、`unmap_range()`、`reserve_mmap_range()`、`release_mmap_range()` 都要维护 holes，否则非 fixed mmap 会选到错误地址。

核心方法地图：

| 方法 | 源码位置 | 作用 |
|------|----------|------|
| `find_user_vma_key()` | `vma_set.rs:188` | 缺页/uaccess 查找覆盖 VPN 的用户 VMA。 |
| `expand_growsdown_for_fault()` | `vma_set.rs:202` | 栈类 VMA 在 guard 规则允许时向下扩展。 |
| `insert_vma()` | `vma_set.rs:287` | 插入新 VMA，并更新用户面积统计和 mmap holes。 |
| `split_for_range()` | `vma_set.rs:327` | munmap/mprotect/madvise 前把 VMA 切成目标范围边界。 |
| `unmap_range()` | `vma_set.rs:405` | 取消映射，释放 VMA 页和 PTE。 |
| `protect_range()` | `vma_set.rs:581` | 修改权限并同步已存在 PTE。 |
| `find_free_mmap_range()` | `vma_set.rs:632` | 非 fixed mmap 选洞。 |
| `try_merge_lazy_private_mmap()` | `vma_set.rs:727` | 合并相邻 lazy private anonymous VMA。 |

如果某个 mmap 地址选择异常，优先检查 `mmap_holes`；如果 fault 找不到 VMA，优先检查 `vmas` 的范围和 split/merge；如果 `/proc/[pid]/maps` 统计异常，优先检查 `user_area_count/user_page_count`。

`mmap` 的地址选择、覆盖和 VMA 插入由 `mmap.rs::do_mmap()` 完成：

```rust
pub(super) fn do_mmap<T: PageTable>(
    address_space: &mut AddressSpace<T>,
    start: usize,
    len: usize,
    prot: MapPermission,
    flags: MapFlags,
    offset: usize,
    map_file: Option<Arc<dyn IndexNode>>,
    may_write: bool,
    write_sealed: bool,
) -> isize {
    if start & 0xfff != 0 {
        return EINVAL;
    }
    let (start_hint, requested_end) = match checked_user_range(start, len) {
        Ok(range) => range,
        Err(errno) => return errno,
    };
    if charges_overcommit(prot, flags)
        && !crate::mm::overcommit_allows(address_space.committed_bytes(), len)
    {
        return ENOMEM;
    }
    let fixed =
        flags.contains(MapFlags::MAP_FIXED) || flags.contains(MapFlags::MAP_FIXED_NOREPLACE);
    let start_va: VirtAddr = if fixed {
        let start_vpn = start_hint.floor();
        let end_vpn = requested_end.ceil();
        if flags.contains(MapFlags::MAP_FIXED_NOREPLACE)
            && address_space.vmas.has_overlap(start_vpn, end_vpn)
        {
            return EEXIST;
        }
        if let Err(errno) =
            address_space
                .vmas
                .unmap_range(&mut address_space.page_table, start_vpn, end_vpn, true)
        {
            return errno;
        }
        address_space.set_locked_pages(start_vpn, end_vpn, false);
        start_hint
    } else {
        let hinted_start = if start != 0 {
            let start_vpn = start_hint.floor();
            let end_vpn = requested_end.ceil();
            if address_space.vmas.is_mmap_range_free(start_vpn, end_vpn) {
                Some(start_hint)
            } else {
                None
            }
        } else {
            None
        };
        let start_va = match hinted_start {
            Some(start_va) => start_va,
            None => match address_space.vmas.find_free_mmap_range(len, PAGE_SIZE) {
                Ok(start_va) => start_va,
                Err(errno) => return errno,
            },
        };
        if !flags.contains(MapFlags::MAP_LOCKED) {
            match address_space
                .vmas
                .try_merge_lazy_private_mmap::<T>(start_va, len, prot, flags)
            {
                Ok(Some(end_va)) => return end_va.0 as isize,
                Ok(None) => {}
                Err(errno) => return errno,
            }
        }
        start_va
    };
    let end = match start_va.0.checked_add(len) {
        Some(end) => end,
        None => return EINVAL,
    };
    let end_va = VirtAddr::from(end);
    let start_vpn = start_va.floor();
    let end_vpn = end_va.ceil();
    if address_space.vmas.has_overlap(start_vpn, end_vpn) {
        return EINVAL;
    }
    if let Err(errno) = address_space.vmas.try_reserve(1) {
        return errno;
    }
    let mut new_area = match Vma::try_new(start_va, end_va, prot, None, 0) {
        Ok(area) => area,
        Err(e) => return e,
    };
    new_area.flags = flags;
    new_area.may_write = may_write;
    new_area.write_sealed = write_sealed;
    if !flags.contains(MapFlags::MAP_ANONYMOUS) {
        if offset & (PAGE_SIZE - 1) != 0 || offset > isize::MAX as usize {
            return EINVAL;
        }
        let Some(inode) = map_file else {
            return EBADF;
        };
        new_area.map_file = Some(inode);
        new_area.map_file_offset = offset;
    }
```

后半段处理 writable anonymous `MAP_SHARED` 预分配 frame、插入 VMA、`MAP_LOCKED` 标记并返回起始地址：

```rust
    if flags.contains(MapFlags::MAP_SHARED)
        && new_area.map_file.is_none()
        && new_area.map_perm.contains(MapPermission::W)
    {
        if len > MAX_EAGER_MMAP_SIZE {
            return ENOMEM;
        }
        let vpn_range = new_area.inner.vpn_range;
        for vpn in vpn_range {
            if let Err(err) = new_area.alloc_one_zeroed_unmapped(vpn) {
                return match err {
                    MemoryError::OutOfMemory => ENOMEM,
                    _ => EINVAL,
                };
            }
        }
    }

    if let Err(errno) = address_space.vmas.insert_vma(new_area) {
        return errno;
    }
    if flags.contains(MapFlags::MAP_LOCKED) {
        address_space.set_locked_pages(start_vpn, end_vpn, true);
    }

    start_va.0 as isize
}
```

非 fixed mmap 优先使用可用 hint；hint 不可用时从 `mmap_holes` 中找空洞。lazy private anonymous 映射可尝试合并，`MAP_LOCKED` 不走该合并路径。

## 4. VMA 查询路径

按地址查找 VMA 的核心路径：

```text
VirtAddr
  └── floor() -> VirtPageNum
        └── VmaSet::find_user_vma_key(vpn)
              ├── vmas.range(..=vpn).next_back()
              ├── area.vm_contains(vpn)
              └── area.vm_is_user()
```

`find_user_vma_key()` 只返回用户 VMA。trap context 等非用户映射不会被用户缺页路径当成合法访问范围。

## 5. ELF 地址空间创建

`AddressSpaceInner::from_elf_inode(file)` 是 exec/initproc 的首选路径：

1. 只读取 ELF header/program header，校验 PT_LOAD 虚拟范围、文件范围与解释器。
2. 按 VPN 合并重叠段的 `R/W/X/U` 权限，建立连续的 `ElfLazy` VMA，但不分配 PT_LOAD frame。
3. 所有 VMA 共享 `ElfLazyBacking`，保存 PageCache、可执行 inode 生命周期和按 program-header 顺序的覆盖配方。
4. `PT_INTERP` 递归使用同一路径，因此动态解释器也不在 exec 时全量装入。
5. 设置 `heap_bottom = program_break`、`heap_pt = program_break`。

文件系统无 PageCache 时返回 `ENOSYS`，再由 `AddressSpaceInner::from_elf(elf_data)`
执行兼容 eager 路径：

1. 调用 `AddressSpaceInner::new_bare()` 创建空页表与空 VMA 集合。
2. 按架构需要映射 trampoline。
3. 映射 signal trampoline。
4. 解析 ELF。
5. `map_elf()` 遍历 program header。
6. LOAD 段转换成 VMA 并立即分配、清零、写入初始数据。
7. INTERP 段递归加载动态解释器。
8. 设置 `heap_bottom = program_break`、`heap_pt = program_break`。

LOAD 段权限来自 `MapPermission::from_ph_flags(ph.flags())`。ELF 类型处理如下：

| ELF 类型 | bias |
|----------|------|
| `Executable` | `0` |
| `SharedObject` 且无 `PT_INTERP` | `ELF_DYN_BASE` |
| `SharedObject` 且一个 `PT_INTERP` | `ELF_PIE_BASE` |
| 其他或多个解释器 | `ENOEXEC` / `EINVAL` |

相邻 `PT_LOAD` 段允许在同一个向上取整的页中相接或重叠。加载器先验证每段的派生范围与文件范围，再按 VPN 去重并合并 `R/W/X/U` 权限；连续且权限相同的页组成一个 VMA。每页只映射和清零一次，随后仍按 program header 顺序覆盖文件字节，因此后一段在共享页中的字节保持 ELF 规定的顺序。无效范围返回 `ENOEXEC`，页帧或 VMA 容量不足返回 `ENOMEM`。

按需路径保留同样的页级权限和覆盖顺序；差别只在于清零、文件字节覆盖和
PTE 安装延迟到该页的首次 fault。`elf_lazy` 后备在 `Vma::try_clone()`、`from_another()`
和 `into_two()` 中保留，因此 fork、VMA 分裂和后续缺页不会丢失源 inode 或段配方。

## 6. PT_LOAD 共享页映射

该规则同时适用于 boot initproc 和 inode-backed ELF 加载路径，避免同一合法共享页被重复 `Vma::map_one()` 并错误转换为 `ENOEXEC`。

## 7. 用户栈与 trap context

任务资源由 `alloc_user_res_with_trap_ppn(slot, alloc_stack)` 分配：

| 资源 | 地址来源 | 映射 |
|------|----------|------|
| 用户栈 | `ustack_bottom_from_slot(slot)` | `MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK` |
| trap context | `trap_cx_bottom_from_slot(slot)` | `R | W`，非用户页 |

用户栈 VMA 的完整范围是 `USER_STACK_SIZE`，但初始只映射 `USER_STACK_INIT_SIZE`。这使栈增长依赖缺页路径和 VMA 范围，而不是启动时预分配全部栈页。

## 8. proc maps 输出

`AddressSpaceInner::proc_maps_content()` 遍历用户 VMA，输出类似：

```text
0000000000010000-0000000000020000 rw-p 00000000 00:00 0
```

权限字符由 `MapPermission` 决定：

| 字符 | 条件 |
|------|------|
| `r` | `R` |
| `w` | `W` |
| `x` | `X` |
| `s` | `VmAreaMapping::Shared` |
| `p` | `VmAreaMapping::Private` |

`proc_smaps_read_cursor()`（配合 per-open `vfs::SmapsCursor`）进一步按段输出 Size/Rss/Pss/Locked 等字段，每次 read(2) 只生成一个 VMA 段，避免为大量 VMA 构建完整快照。Rss 来自 `VmPageStore::in_memory_len_in_range()`，不是 VMA 总长度。

## 9. VMA 分裂

`mprotect`、`munmap`、`madvise` 等对范围生效的操作会调用 `VmaSet::split_for_range()`。

分裂策略：

| 目标范围 | 新增 VMA 数 |
|----------|-------------|
| 刚好覆盖整段 | 0 |
| 覆盖左半或右半 | 1 |
| 覆盖中间 | 2 |

`Vma::into_two()` 会同步处理：

1. VPN 范围切分。
2. 文件映射偏移调整。
3. `VmPageStore` 中每页状态按 VPN 分裂。
4. OOM handler 特性下 active 队列和压缩/换出计数重算。

`split_for_range()` 是这些范围操作的共同前置步骤：

```rust
pub(super) fn split_for_range(
    &mut self,
    area_start: VirtPageNum,
    start_vpn: VirtPageNum,
    end_vpn: VirtPageNum,
) -> Result<VirtPageNum, isize> {
    let area = self.vmas.get(&area_start).ok_or(EINVAL)?;
    let split_is_user = area.vm_is_user();
    let area_start_vpn = area.vm_start();
    let area_end_vpn = area.vm_end();
    if start_vpn < area_start_vpn || end_vpn > area_end_vpn || start_vpn >= end_vpn {
        return Err(EINVAL);
    }
    let additional = if start_vpn == area_start_vpn && end_vpn == area_end_vpn {
        0
    } else if start_vpn == area_start_vpn || end_vpn == area_end_vpn {
        1
    } else {
        2
    };
    self.try_reserve(additional)?;
    let mut area = self.vmas.remove(&area_start).ok_or(EINVAL)?;
    if start_vpn == area_start_vpn && end_vpn == area_end_vpn {
        self.vmas.insert(area_start_vpn, area);
        self.debug_assert_invariants();
        Ok(area_start_vpn)
    } else if start_vpn == area_start_vpn {
        let second = match area.into_two(end_vpn) {
            Ok(second) => second,
            Err(_) => {
                self.vmas.insert(area_start_vpn, area);
                return Err(EINVAL);
            }
        };
        let target_start = area.vm_start();
        self.insert_split_piece(area);
        self.insert_split_piece(second);
        if split_is_user {
            self.user_area_count += 1;
        }
        self.debug_assert_invariants();
        Ok(target_start)
    } else if end_vpn == area_end_vpn {
        let second = match area.into_two(start_vpn) {
            Ok(second) => second,
            Err(_) => {
                self.vmas.insert(area_start_vpn, area);
                return Err(EINVAL);
            }
        };
        let target_start = second.vm_start();
        self.insert_split_piece(area);
        self.insert_split_piece(second);
        if split_is_user {
            self.user_area_count += 1;
        }
        self.debug_assert_invariants();
        Ok(target_start)
    } else {
        let (second, third) = match area.into_three(start_vpn, end_vpn) {
            Ok(parts) => parts,
            Err(_) => {
                self.vmas.insert(area_start_vpn, area);
                return Err(EINVAL);
            }
        };
        let target_start = second.vm_start();
        self.insert_split_piece(area);
        self.insert_split_piece(second);
        self.insert_split_piece(third);
        if split_is_user {
            self.user_area_count += 2;
        }
        self.debug_assert_invariants();
        Ok(target_start)
    }
}
```

完整覆盖时不增加 VMA；覆盖边缘时一分为二；覆盖中间时一分为三。函数返回被操作目标段的起始 VPN，供 `munmap/mprotect/madvise` 继续定位。

## 10. max_map_count

`VmaSet::ensure_can_add()` 使用 `crate::mm::max_map_count()` 限制用户 VMA 数量：

```rust
len > crate::mm::max_map_count().saturating_add(1)
```

实现注释说明这里模拟 Linux 的失败点：可见 map count 可以在失败点超过 `max_map_count` 一个单位；内部非用户 VMA 不计入该限制。

## 11. MAP_GROWSDOWN

栈类 VMA 的向下增长由 `expand_growsdown_for_fault()` 实现。缺页地址不在任何 VMA 内时，缺页路径会尝试扩展紧邻其上的 `MAP_GROWSDOWN` VMA。

拒绝条件包括：

| 条件 | 结果 |
|------|------|
| fault_vpn 不在 VMA 起点以下 | 不扩展 |
| VMA 非用户或不含 `MAP_GROWSDOWN` | 不扩展 |
| fault gap 大于 `USER_STACK_SIZE / PAGE_SIZE` | 不扩展 |
| 与前一个 VMA 的 guard gap 小于 256 页 | 不扩展 |
| 新范围与现有 VMA 重叠 | 不扩展 |

扩展成功时会 reserve mmap hole、更新 VMA 起点、调整用户页计数。

## 12. locked pages

`locked_pages` 是地址空间级的页集合。相关接口：

| 接口 | 行为 |
|------|------|
| `mlock` | 校验范围，逐页 fault-in，再标记 locked |
| `mlock_onfault` | 校验范围，只标记 locked，不立即 fault-in |
| `munlock` | 清除指定范围 |
| `mlockall_current` | 标记所有用户 VMA |
| `munlockall` | 清空集合 |

`munmap` 成功后会清理对应范围的 locked 标记。`madvise(MADV_DONTNEED)` 对 locked 页返回 `EINVAL`。

## 13. 地址空间释放

进程退出后，外层 `AddressSpace::write()` 调用
`AddressSpaceInner::release_for_zombie()` 释放大部分 MM 资源：

```rust
self.with_user_mapper(|vmas, mapper| vmas.unmap_all(mapper))
    .expect("zombie cleanup failed to clear a resident user PTE");
self.locked_pages.clear();
self.mmu_gather.record_full_flush();
```

这一步在 idle 栈已经换走 SATP 根、active mask 为零且 VM 未共享后撤销 resident PTE。
用户 frame 由 `MmuGather` 持有到 TLB ack；页表根/中间页保留到 `AddressSpace` 最终
Arc drop，避免仍安装旧根的 CPU 在 ack 后 page walk 已释放页表。它不销毁进程等待
元数据，僵尸进程仍可被 `wait4/waitid` 收集。

`AddressSpace` 的生命周期和 PCB 不完全相同。进程进入 zombie 后，父进程 wait 仍需要 pid、exit code、rusage、children 等 PCB 元数据；提前释放只针对用户 VMA/叶子 frame，少量页表结构继续随 VM 存活到 reap。

读 VMA bug 时建议同时检查三张结构：`VmaSet` 的 BTreeMap 是否覆盖目标 VPN，`mmap_holes` 是否正确反映空洞，`VmPageStore` 是否记录 resident/unallocated 状态。`proc_maps` 只看 VMA 范围，`mincore` 和 RSS 还要看 PTE/PageCache/resident frame，因此两个接口输出不同不一定是 bug。

## 14. 关键约束

1. VMA key 必须是起始 VPN，范围不能重叠。
2. 用户访问路径必须使用 `find_user_vma_key()`，不能把非用户 VMA 当成用户可访问。
3. VMA 分裂后必须维护 `user_area_count` 和 `user_page_count`。
4. 文件 VMA 切分时必须同步调整第二段 `map_file_offset`。
5. `MAP_GROWSDOWN` 扩展必须保留 guard gap。
6. `proc_maps` 的 RSS 统计来自 resident frame 数，不等于虚拟映射大小。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| `mprotect` 后相邻 VMA 混乱 | `split_for_range()` 是否正确生成 1/2 个新段 |
| `/proc/[pid]/maps` 权限错误 | `map_perm` 与 `VmAreaMapping` 是否更新 |
| 栈缺页返回 EFAULT | `MAP_GROWSDOWN` gap、guard gap、VMA 位置 |
| 大量 mmap 后 ENOMEM | `max_map_count`、`mmap_holes`、overcommit 三者分别核对 |
| zombie 进程仍占大量内存 | VM 是否未共享、active mask 是否清零、idle 收尾是否执行提前释放 |
