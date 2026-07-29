---
title: "内存管理架构详解 (Memory Management Architecture)"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-07-27
tags: [mm, address-space, vma, mmap, page-fault, cow, mmu-gather]
---

# 内存管理架构详解

## 1. 概述

MangoCore 内存管理由内核地址空间、物理页分配器、页表抽象、用户地址空间、VMA 集合、缺页处理、文件映射和用户内存访问组成。架构无关层通过 `PageTable` trait 操作页表，具体后端由 HAL 提供：rv64 使用 `Sv39PageTable`，la64 使用 `LAFlexPageTable`。

MM 的核心对象是 `AddressSpace<PageTableImpl>`。每个进程持有一个地址空间对象；`clone` 根据 `CLONE_VM` 决定共享或复制；`execve` 构造新地址空间并替换进程 VM。

## 2. 设计目标

| 目标 | 实现方式 |
|------|----------|
| 架构无关页表操作 | `PageTable` trait + HAL `PageTableImpl` |
| 每进程独立地址空间 | `ProcessControlBlock::vm` 指向 `AddressSpace<PageTableImpl>` |
| VMA 精确管理 | `VmaSet` 使用 `BTreeMap<VirtPageNum, Vma>` 管理范围 |
| lazy allocation | 匿名和文件映射按需缺页安装 PTE |
| Linux mmap 语义 | 支持 private/shared、fixed/noreplace、file/anonymous、growdown、mprotect、madvise |
| fork CoW | fork 撤销私有可写页 W 权限，写缺页复制或恢复 |
| 文件映射一致性 | file-backed fault 通过 inode PageCache |
| 用户指针安全 | uaccess 先 fault-in，再校验页表权限 |
| PTE/TLB 一致性 | PTE 修改通过页表接口刷新 TLB |

## 3. 架构

### 3.1 层次

```
+-------------------------------------------------------------------+
| syscall/process/mm.rs                                             |
| brk mmap munmap mprotect mremap mincore madvise mlock process_vm  |
+-------------------------------------------------------------------+
| AddressSpace<PageTableImpl>                                       |
| page_table | VmaSet | heap_bottom | heap_pt | locked_pages        |
+-------------------------------------------------------------------+
| VmaSet                              | Vma                          |
| BTreeMap<VPN,Vma>                   | VmPageStore                  |
| mmap holes / split / merge          | perms / file / flags         |
+-------------------------------------------------------------------+
| page_fault.rs                       | filemap.rs                   |
| classify fault action               | page cache backed fault      |
+-------------------------------------------------------------------+
| PageTable trait                     | Frame allocator/store        |
| map/unmap/flags/TLB                 | FrameTracker / VmPageStore   |
+-------------------------------------------------------------------+
| HAL PageTableImpl                   | hardware TLB                 |
+-------------------------------------------------------------------+
```

### 3.2 源文件地图

| 文件 | 职责 |
|------|------|
| `mm/mod.rs` | MM 模块声明和初始化 |
| `mm/kernel_space.rs` | 内核地址空间、内核段和 MMIO 映射 |
| `mm/kernel_mapper.rs` | 内核页表 mapper |
| `mm/frame_allocator.rs` | 物理页分配器和 `FrameTracker` |
| `mm/frame_store.rs` | VMA 内页状态 `VmPageStore` |
| `mm/page_table.rs` | `PageTable` trait、`FaultAccess`、`UserAccess` |
| `mm/address_space.rs` | `AddressSpace`、ELF、fault-in、mmap wrapper、fork VM |
| `mm/vma.rs` | 单个 VMA、CoW、unmap、权限恢复 |
| `mm/vma_set.rs` | VMA range 管理、split/merge、mprotect/mincore/madvise |
| `mm/mmap.rs` | `do_mmap()`、`do_sbrk()` |
| `mm/page_fault.rs` | 缺页动作分类和执行 |
| `mm/filemap.rs` | file-backed fault |
| `mm/uaccess.rs` | 用户内存访问 |
| `syscall/process/mm.rs` | MM syscall 参数解析 |

## 4. 关键数据结构

### 4.1 AddressSpace

| 字段 | 说明 |
|------|------|
| `page_table` | 架构页表实现 |
| `vmas` | `VmaSet` |
| `heap_bottom` | heap 起始地址 |
| `heap_pt` | 当前 program break |
| `locked_pages` | mlock/mlockall 相关计数 |

重要方法：

| 方法 | 说明 |
|------|------|
| `new_bare()` | 创建空用户地址空间 |
| `from_elf()` | 构造 ELF 地址空间 |
| `from_existing_user()` | fork/clone 复制用户 VM |
| `do_page_fault()` | 缺页入口 |
| `fault_in_user_va()` | syscall/uaccess 路径 fault-in |
| `mmap/munmap/mprotect/madvise/mincore` | VMA 操作 wrapper |
| `sbrk/brk` | heap 调整 |

### 4.2 VmaSet

`VmaSet` 用 `BTreeMap` 以 VMA 起始 VPN 为 key。

| 功能 | 方法 |
|------|------|
| 查询 | `find_vma_key()`, `find_user_vma_key()`, `find_user_vma_mut()` |
| split | `split_for_range()` |
| unmap | `unmap_range()` |
| protect | `protect_range()` |
| advice | `advise_range()` |
| resident | `mincore_range()` |
| growdown | `expand_growsdown_for_fault()` |
| holes | `find_free_mmap_range()`, reserve/release hole |
| merge | `try_merge_lazy_private_mmap()` |

rv64 mmap 范围使用 `MMAP_BASE/MMAP_END`；la64 使用 `USR_MMAP_BASE/USR_MMAP_END`。

### 4.3 Vma

| 字段 | 说明 |
|------|------|
| `inner` | `VmPageStore`，记录 VPN 到 frame 状态 |
| `map_perm` | VMA 权限 |
| `map_file` | 文件映射 inode |
| `map_file_offset` | 文件映射偏移 |
| `may_write` | 是否允许后续恢复/授予写权限 |
| `write_sealed` | memfd seal 影响 |
| `flags` | mmap flags |
| `wipe_on_fork` | fork 时清空 |
| `dont_fork` | fork 时跳过 |
| `fork_inherited` | fork 继承状态 |

### 4.4 Frame 和 VmPageStore

| 类型 | 说明 |
|------|------|
| `FrameTracker` | 物理页 RAII 对象，创建时清零，drop 时归还 |
| `Frame::InMemory` | VPN 已拥有物理页 |
| `Frame::Unallocated` | lazy 状态 |
| `VmPageStore` | VMA 内 VPN 到 frame 状态、resident 统计 |

启用 OOM/swap 相关 feature 时，`Frame` 还包含压缩/换出状态。

### 4.5 PageTable trait

| 类别 | 方法 |
|------|------|
| 创建/激活 | `new`, `new_kern_space`, `from_token`, `activate`, `token` |
| 映射 | `try_map`, `map`, `unmap`；`UserMapper` 使用对应 raw/no-flush 原语 |
| 查询 | `translate`, `translate_va`, `is_mapped`, `is_valid` |
| 权限 | `readable`, `writable`, `executable`, `user_access_ok` |
| 修改 | `set_ppn`, `set_pte_flags`, `revoke_*`, `clear_access`, `clear_dirty`；用户 PTE 写入经 `UserMapper` |
| CoW | `UserMapper::block_write` 内部调用 `block_and_ret_mut_no_flush` |
| TLB | `flush_tlb_page`, `flush_tlb`；用户路径由 `MmuGather` 合并失效范围 |

## 5. 执行流程

### 5.1 MM 初始化

```
heap_allocator::init_heap()
heap_trace::enable()                   [heap_trace]
frame_allocator::init_frame_allocator()
KERNEL_SPACE.lock().activate()
```

`KernelSpace::new()` 映射 trampoline、内核段、物理内存和 MMIO：

| 区域 | 权限 |
|------|------|
| `.text` | `R | X | G` |
| `.rodata` | `R` |
| `.data` | `R | W | G` |
| `.bss` | `R | W | G` |
| 每个 usable DRAM region | `R | W | G` |
| `MMIO` | `R | W | G` |

### 5.2 ELF 地址空间

```
AddressSpaceInner::from_elf()
    new_bare()
    map_trampoline()
    map_signal_trampoline()
    map_elf()
    set heap_bottom / heap_pt
```

`map_elf()` 处理：

| ELF 情况 | 行为 |
|----------|------|
| executable | bias 为 0 |
| shared object 无 interp | 使用 `ELF_DYN_BASE` |
| shared object 有 interp | 使用 `ELF_PIE_BASE` 并递归加载解释器 |
| LOAD 段超过 1 GiB | 返回 `ENOMEM` |
| 只读且页对齐 LOAD | 可从内核区域映射 |
| 其他 LOAD | 分配并复制数据 |

### 5.3 用户栈

`insert_user_stack_area()` 创建：

```
[stack_bottom - USER_STACK_SIZE, stack_bottom)
MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK
```

只映射初始 `USER_STACK_INIT_SIZE`，其余栈空间通过后续缺页和 growdown 路径处理。

### 5.4 mmap

```
sys_mmap()
    parse prot
    parse flags
    validate file fd if non-anonymous
    translate /dev/zero
    check memfd seal
do_mmap()
    validate range
    choose address
    handle fixed/noreplace
    create VMA
    prealloc anonymous shared writable frames
    insert VMA
```

关键返回：

| 条件 | errno |
|------|-------|
| 非匿名坏 fd | `EBADF` |
| `MAP_SHARED_VALIDATE` 未知 bit | `EOPNOTSUPP` |
| 文件不可读 | `EACCES` |
| shared writable 文件不可写 | `EACCES` |
| 非 regular 且不是 `/dev/zero` | `EACCES` |
| fixed_noreplace 覆盖已有 VMA | `EEXIST` |
| eager shared anonymous 超过上限 | `ENOMEM` |

### 5.5 brk / sbrk

heap 范围限制：

```
[heap_bottom, heap_bottom + USER_HEAP_SIZE]
```

增长使用匿名私有 fixed mmap，收缩使用 `munmap`。增长前进行 overcommit 检查，并拒绝与不兼容 VMA 重叠。

### 5.6 缺页处理

```
do_page_fault(addr, access, instr)
    find covering VMA
    or expand MAP_GROWSDOWN
    handle_page_fault()
        check_area_permission()
        classify FaultAction
        execute action
```

`FaultAction`：

| 动作 | 行为 |
|------|------|
| `LazyAlloc` | 匿名页首次访问，分配零页 |
| `FileBackedRead` | 从 PageCache 映射文件页 |
| `FileBackedWrite` | 私有文件写缺页，复制到私有 frame |
| `FileBackedSharedWrite` | shared 文件写缺页，通过 PageCache 标脏 |
| `Cow` | 私有页写缺页，复制或恢复权限 |
| `SharedWrite` | 共享页写缺页，恢复写权限 |
| `StaleLazyPte` | 修复 lazy PTE 状态 |
| `MappedRead` | 已映射读路径 |
| `ResidentWithoutPte` | frame store 有页但 PTE 缺失，重新安装 |

### 5.7 fork CoW

```
from_existing_user()
    for each VMA:
        dont_fork -> skip
        wipe_on_fork -> empty VMA
        else clone metadata
             map existing pages
             revoke W for private writable pages
```

写缺页：

```
copy_on_write()
    if frame effectively exclusive:
        restore flags
    else:
        allocate new frame
        copy old bytes
        set PPN and flags
```

`MAP_SHARED` 文件映射不使用私有 CoW。shared write 进入 PageCache dirty 路径。

### 5.8 file-backed fault

| 路径 | 行为 |
|------|------|
| `filemap_read_fault()` | 通过 inode PageCache 获取页，EOF 尾部清零，可写映射清 W |
| 私有写 fault | 从文件内容构造私有 frame |
| `filemap_shared_write_fault()` | 获取 PageCache 可写页并标 dirty |

### 5.9 uaccess

```
translated_byte_buffer()
    check range
    for each page:
        fault_in_user_va()
        check PTE permission
        return page slice
```

单个对象访问不允许跨页；字符串扫描和 buffer 翻译有 8 MiB 上限，iovec 数量上限为 1024。

### 5.10 `AddressSpace::do_page_fault()` 源码路径

`AddressSpace::do_page_fault()` 是 trap 缺页和 uaccess fault-in 的共同入口。它的核心逻辑可以拆成四步：

```rust
let vpn = addr.floor();
let area_start = match self.vmas.find_user_vma_key(vpn) {
    Some(start) => Some(start),
    None => self.vmas.expand_growsdown_for_fault(vpn)?,
};
let area = self.vmas.find_user_vma_mut(vpn).unwrap();
let pa = page_fault::handle_page_fault(area, &mut self.page_table, ctx)?;
self.validate_fault_phys_addr(addr, pa)
```

解释如下：

| 步骤 | 说明 |
|------|------|
| `addr.floor()` | fault 发生在字节地址上，MM 的 VMA、page store 和页表都以 VPN 为单位操作。 |
| `find_user_vma_key()` | 只有落在用户 VMA 内的地址才能修复；不在 VMA 内直接是 bad address。 |
| `expand_growsdown_for_fault()` | 栈类 VMA 可因向下访问而扩展，普通 VMA 不会被隐式创建。 |
| `find_user_vma_mut()` | 找到可变 VMA 后，缺页动作会更新 `VmPageStore`、VMA 统计或页表。 |
| `handle_page_fault()` | 先检查 VMA 权限，再分类为 lazy、file-backed、shared write、CoW 等动作。 |
| `validate_fault_phys_addr()` | 即使动作返回物理地址，也要确认它属于可分配 DRAM，拒绝地址空洞、第 0 页和固件 carveout。 |

缺页处理的权限来源是 VMA，而不是硬件 fault 类型本身。写 fault 落在只读 VMA 上会返回 `NoPermission`；写 fault 落在私有可写但 PTE 被 fork 撤销 W 的页上，才会进入 CoW。

### 5.11 `fault_in_user_va()` 与 syscall 用户访问

uaccess 调用 `fault_in_user_va(addr, access)`，它比 trap 缺页多一个后置校验：

```rust
self.do_page_fault(addr, access)
    .and_then(|_| self.validate_user_fault_result(addr, access))
    .map_err(memory_error_to_errno)
```

后置校验做三件事：

| 校验 | 目的 |
|------|------|
| `translate_va(addr)` | 确认 fault 后页表确实能翻译该 VA。 |
| `validate_fault_phys_addr()` | 防止页表指向真实内存范围外的物理地址。 |
| `user_access_ok(vpn, access)` | 确认最终 PTE 具有用户态和读/写/执行权限。 |

这个契约让 syscall 层可以放心分段复制用户 buffer：每个片段在复制前都已确认存在 PTE、权限正确、物理地址有效。如果只做地址范围检查，lazy mmap、CoW 页和被 mprotect 改过权限的页都会出现错误语义。

### 5.12 `page_fault.rs` 的分类表

`PageFaultHandler::classify()` 是缺页语义的中心。分类先看页表是否已有映射，再看 VMA 类型和 `VmPageStore` 状态：

| 条件 | FaultAction | 后续动作 |
|------|-------------|----------|
| PTE 已映射 + load/execute | `MappedRead` | 直接翻译物理地址。 |
| PTE 已映射 + shared store | `SharedWrite` | 对 file shared 先标脏 PageCache，再恢复 W 权限。 |
| PTE 已映射 + stale lazy store | `StaleLazyPte` | 清理陈旧 PTE，匿名页重新分配。 |
| PTE 已映射 + private store | `Cow` | 调用 `Vma::copy_on_write()` 复制私有页。 |
| file-backed + load/execute | `FileBackedRead` | 通过 `filemap_read_fault()` 映射 page cache 页。 |
| file-backed + shared store | `FileBackedSharedWrite` | 通过 `filemap_shared_write_fault()` 映射并标脏。 |
| file-backed + private store | `FileBackedWrite` | 分配私有页并从文件填充。 |
| anonymous + unallocated | `LazyAlloc` | 分配清零页并建立 PTE。 |
| anonymous + in-memory 但无 PTE | `ResidentWithoutPte` | 重新映射已有 frame。 |

读这张表可以还原大多数 MM 行为：`mmap()` 本身通常只登记 VMA，不一定分配物理页；真正的物理页分配、PageCache 映射或 CoW 发生在 fault action 中。

### 5.13 mmap 生命周期

一次 `mmap()` 从 syscall 到第一次访问会经历两个阶段：

```
sys_mmap()
  -> 参数解析、fd 校验、prot/flags 转换
  -> AddressSpace::mmap()
  -> do_mmap(): 选择地址、保留 hole、创建 VMA
  -> 返回用户虚拟地址

用户首次访问
  -> page fault
  -> 根据 VMA 类型分配 frame 或映射 file page cache
```

这个两阶段设计解释了几个现象：

| 现象 | 原因 |
|------|------|
| `mmap()` 成功不代表物理页已分配 | lazy allocation 把分配推迟到第一次访问。 |
| `mincore()` 可能看到未驻留页 | VMA 存在但 `VmPageStore` 或 PageCache 未 resident。 |
| `MAP_SHARED` 文件写 fault 需要标脏 | 共享写入必须反映到 inode PageCache 状态，供写回路径处理。 |
| `MAP_PRIVATE` 文件写 fault 不改文件页 | private 写入分配匿名私有页，文件 PageCache 只提供初始内容。 |
| `mprotect()` 修改权限后必须影响 PTE | 已存在映射要同步调整页表权限并刷新 TLB。 |

## 6. 接口与 API

### 6.1 syscall API

| syscall | 核心路径 |
|---------|----------|
| `mmap` | `sys_mmap()` -> `do_mmap()` |
| `munmap` | `sys_munmap()` -> `AddressSpace::munmap()` |
| `mprotect` | `sys_mprotect()` -> `VmaSet::protect_range()` |
| `brk/sbrk` | `sys_brk/sys_sbrk()` -> `do_sbrk()` |
| `mincore` | `sys_mincore()` -> `VmaSet::mincore_range()` |
| `madvise` | `sys_madvise()` -> `VmaSet::advise_range()` |
| `mlock*` | `AddressSpace` locked page paths |
| `process_vm_*` | 跨进程用户 buffer 和 VM 访问 |

### 6.2 AddressSpace API

| API | 说明 |
|-----|------|
| `token()` | 页表 token |
| `vma_count()` | VMA 数量 |
| `committed_bytes()` | 提交内存统计 |
| `has_shared_writable_mapping()` | 检查 inode 是否存在 shared writable mapping |
| `proc_maps_string()` / smaps | procfs maps/smaps 输出 |
| `futex_uses_shared_key()` | futex shared key 判定 |

### 6.3 VMA API

| API | 说明 |
|-----|------|
| `map_from_existing_page_table()` | fork 映射已有页 |
| `copy_on_write()` | CoW 写缺页 |
| `unmap()` | 取消映射 |
| `discard_range()` | 丢弃范围 |
| `expand_to()` / `expand_down_to()` | 扩展 VMA |
| `into_two()` / `into_three()` | VMA split |

## 7. 测试映射

| 功能 | 测试来源 |
|------|----------|
| ELF/exec 地址空间 | busybox、libctest、LTP exec |
| brk/sbrk | libc malloc、LTP brk |
| mmap/munmap | LTP mmap、mmapstress、libcbench |
| mprotect | LTP mprotect、权限 fault |
| CoW/fork | fork/clone、copy-on-write 用例 |
| file-backed mmap | iozone、LTP mmap file |
| MAP_SHARED | shared mmap、futex shared key |
| uaccess | syscall 参数、read/write/iovec |
| mincore/mlock/madvise | LTP mm |
| OOM | oom_handler feature 和内存压力测试 |

## 8. 已知边界

| 边界 | 说明 |
|------|------|
| eager shared anonymous | writable anonymous `MAP_SHARED` 预分配 frame，长度超过 `MAX_EAGER_MMAP_SIZE` 返回 `ENOMEM` |
| `remap_file_pages` | 仅做参数和范围校验，最终返回 `EINVAL` |
| pkey | 提供兼容入口，实际权限隔离以现有实现为准 |
| `check_user_range()` | 只做范围/溢出检查，不代表可访问 |
| TLB | PTE 修改必须刷新；fork 批量撤销 W 后统一 flush |
| swap/zram | 相关状态受 feature 控制，普通路径只涉及内存页和未分配页 |

## 9. 源文件索引

| 路径 | 内容 |
|------|------|
| `os/src/mm/mod.rs` | MM 初始化 |
| `os/src/mm/kernel_space.rs` | 内核地址空间 |
| `os/src/mm/frame_allocator.rs` | 物理页分配 |
| `os/src/mm/frame_store.rs` | VMA page store |
| `os/src/mm/page_table.rs` | 页表 trait |
| `os/src/mm/address_space.rs` | 地址空间、ELF、fork、fault |
| `os/src/mm/vma.rs` | VMA、CoW |
| `os/src/mm/vma_set.rs` | VMA 集合 |
| `os/src/mm/mmap.rs` | mmap/brk |
| `os/src/mm/page_fault.rs` | 缺页处理 |
| `os/src/mm/filemap.rs` | 文件映射 fault |
| `os/src/mm/uaccess.rs` | 用户内存访问 |
| `os/src/syscall/process/mm.rs` | MM syscall |
