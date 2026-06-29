---
title: "内存管理子系统 (Memory Management)"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, vma, mmap, page-fault, pagetable]
---

# 内存管理子系统

## 概述

MangoCore 的内存管理由物理页分配器、架构页表实现、进程地址空间、VMA 集合、缺页处理、文件映射和用户内存访问组成。架构无关代码通过 `PageTable` trait 操作页表；具体页表实现由 HAL 提供，rv64 使用 SV39，la64 使用 LoongArch64 后端的 flexible page table。

## 依据范围

| 主题 | 主要源码 |
|------|----------|
| MM 初始化与模块边界 | `os/src/mm/mod.rs` |
| 地址空间 | `os/src/mm/address_space.rs` |
| VMA | `os/src/mm/vma.rs`, `os/src/mm/vma_set.rs` |
| mmap/brk syscall 语义 | `os/src/mm/mmap.rs`, `os/src/syscall/process/mm.rs` |
| 缺页处理 | `os/src/mm/page_fault.rs`, `os/src/mm/filemap.rs` |
| 页表抽象 | `os/src/mm/page_table.rs` |
| 物理页分配与状态 | `os/src/mm/frame_allocator.rs`, `os/src/mm/frame_store.rs` |
| 用户内存访问 | `os/src/mm/uaccess.rs` |

## 架构

```
+-------------------------------------------------------------+
| syscall/process/mm.rs                                       |
| brk, mmap, munmap, mprotect, mremap, mincore, madvise ...   |
+-------------------------------------------------------------+
| AddressSpace<T: PageTable>                                  |
| page_table + VmaSet + heap metadata + locked pages          |
+-------------------------------------------------------------+
| VmaSet                         | Vma                         |
| BTreeMap<VPN, Vma>             | VmPageStore + perms + file  |
| mmap holes + split/merge       | flags + fork attributes     |
+-------------------------------------------------------------+
| page_fault.rs + filemap.rs                                  |
| lazy anon | file read/write | shared write | CoW             |
+-------------------------------------------------------------+
| PageTable trait + PageTableImpl                              |
| rv64 Sv39PageTable | la64 LAFlexPageTable                    |
+-------------------------------------------------------------+
| frame_allocator.rs + frame_store.rs                          |
| FrameTracker | StackFrameAllocator | VmPageStore             |
+-------------------------------------------------------------+
```

## 初始化

`mm::init()` 的当前顺序是：

```
heap_allocator::init_heap()
frame_allocator::init_frame_allocator()
KERNEL_SPACE.lock().activate()
```

如果启用堆追踪特性，`heap_trace::enable()` 会在堆初始化后执行。物理页分配器从 `ekernel` 到 `MEMORY_END` 建立可分配区间；内核地址空间激活后，后续文件系统、驱动和任务初始化运行在内核页表之上。

## 核心数据结构

| 结构 | 文件 | 作用 |
|------|------|------|
| `AddressSpace<T>` | `address_space.rs` | 每进程地址空间，包含页表、VMA 集合、堆边界和 locked page 计数 |
| `VmaSet` | `vma_set.rs` | 用 `BTreeMap<VirtPageNum, Vma>` 管理 VMA、mmap holes 和用户映射统计 |
| `Vma` | `vma.rs` | 单段映射，包含权限、文件后端、fork 行为和 `VmPageStore` |
| `VmPageStore` | `frame_store.rs` | 记录每个 VPN 的物理页状态 |
| `FrameTracker` | `frame_allocator.rs` | 物理页 RAII 包装，drop 时归还页帧 |
| `PageTable` | `page_table.rs` | 架构无关页表操作 trait |

## 功能矩阵

| 功能 | 实现状态 |
|------|----------|
| 用户 ELF 映射 | `AddressSpace::from_elf()` 与 `map_elf()` 映射 LOAD、INTERP、trampoline 和 signal trampoline |
| 用户栈 | `insert_user_stack_area()` 建立 `MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK` VMA，只映射初始栈页 |
| heap | `do_sbrk()` 基于匿名私有固定 mmap 增长，通过 `munmap` 收缩 |
| mmap | 支持匿名/文件、shared/private、fixed/fixed_noreplace、lazy allocation 和 shared anonymous 预分配 |
| munmap | `VmaSet::unmap_range()` 分裂 VMA 并释放 mmap hole |
| mprotect | `VmaSet::protect_range()` 校验写权限、文件 seal 和 VMA 范围后更新权限 |
| mincore | 检查 PTE 映射或文件页是否在 page cache 中 |
| madvise | 覆盖 DONTNEED、FREE、DONTFORK、DOFORK、WIPEONFORK、KEEPONFORK 等已实现分支 |
| CoW | fork 时撤销私有可写映射的 W 权限；写缺页时复制或恢复权限 |
| MAP_SHARED | 文件共享写通过 page cache 标脏；匿名 shared 使用共享帧 |
| 用户指针 | `uaccess.rs` 提供 checked translation、fault-in 和拷贝封装 |

## 文档索引

| 文档 | 内容 |
|------|------|
| `README.md` | MM 总览、初始化、核心结构 |
| `architecture.md` | MM 架构详解，覆盖 AddressSpace/VMA/mmap/缺页/CoW/filemap/uaccess/TLB |
| `initialization-and-kernel-space.md` | MM 初始化、内核地址空间、内核段和 MMIO 映射 |
| `frame-allocator.md` | 物理页分配器、FrameTracker、OOM 分配尝试 |
| `page-table-and-tlb.md` | PageTable trait、访问类型、PTE 修改和 TLB 约束 |
| `address-space-and-vma.md` | 地址空间、ELF、mmap/brk、fork VMA 语义 |
| `mmap-and-brk.md` | mmap/brk 参数解析、地址选择、anonymous shared 预分配 |
| `page-fault-and-usercopy.md` | 缺页分类、CoW、文件映射和用户内存访问 |
| `frame-and-pagetable.md` | 物理页分配、Frame store、页表 trait 和 TLB 约束 |
| `cow-and-fork.md` | fork 地址空间、私有 CoW、shared 映射和 madvise fork 标记 |
| `filemap-and-page-cache.md` | 文件映射缺页与 PageCache 交互 |
| `uaccess.md` | 用户指针、字符串、iovec、fault-in 和权限检查 |
| `oom-and-locking.md` | OOM、overcommit、locked pages 和防御性限制 |
| `debugging.md` | MM 状态地图、缺页/mmap/CoW/OOM 调试路径和测试映射 |
