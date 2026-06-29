---
title: "内存管理调试与测试映射"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, debug, page-fault, mmap, cow, test]
---

# 内存管理调试与测试映射

## 1. 源码位置

| 源码 | 调试对象 |
|------|----------|
| `os/src/mm/address_space.rs` | 地址空间、缺页入口、uaccess fault-in 后置校验 |
| `os/src/mm/vma.rs`、`os/src/mm/vma_set.rs` | VMA 字段、分裂/合并、mmap holes、CoW |
| `os/src/mm/mmap.rs` | mmap/brk/munmap/mprotect 主逻辑 |
| `os/src/mm/page_fault.rs` | `FaultAction` 分类和缺页动作分派 |
| `os/src/mm/filemap.rs` | 文件 mmap 缺页和 PageCache 交互 |
| `os/src/mm/frame_allocator.rs`、`os/src/mm/frame_store.rs` | 物理页分配、frame 状态、OOM 回收 |
| `os/src/mm/uaccess.rs` | 用户指针、buffer、iovec、copy_from/to_user |

## 2. MM 状态地图

MM 调试要同时看三类状态：

| 状态 | 结构 | 作用 |
|------|------|------|
| 虚拟范围 | `VmaSet`, `Vma` | 地址是否合法、权限是什么、文件/匿名、shared/private。 |
| 页状态 | `VmPageStore`, `FrameTracker` | 页面是否 resident、是否共享、是否 lazy、是否 compressed/swapped。 |
| 硬件翻译 | `PageTable`, TLB | 当前 CPU 实际如何翻译 VA，权限是否已同步。 |

只看其中一层容易误判：VMA 存在不代表 PTE 存在，PTE 存在不代表 VMA 允许访问，frame resident 不代表用户页表已经映射。

## 3. 缺页定位

```
trap/uaccess
  -> AddressSpace::do_page_fault()
  -> VmaSet::find_user_vma_key()
  -> page_fault::handle_page_fault()
  -> Vma / filemap / PageTable
```

| 现象 | 检查 |
|------|------|
| 地址直接 bad address | VMA 是否覆盖 VPN，是否是用户 VMA |
| 写只读页未报错 | `Vma::vm_allows(Store)` 和 PTE 权限 |
| fork 后写没有 CoW | private writable 页 fork 时是否撤销 W，TLB 是否 flush |
| MAP_SHARED 写未回写 | filemap shared write 是否调用 PageCache dirty 路径 |
| 文件 mmap 尾页脏数据 | `zero_tail()` 和 private/cache frame 清零 |
| la64 写 fault 重复 | dirty bit 设置和 TLB invalidate |
| uaccess EFAULT | `fault_in_user_va()` 后置权限检查 |

## 4. mmap/brk 定位

| 症状 | 源码入口 | 重点状态 |
|------|----------|----------|
| 返回地址不符合预期 | `mmap.rs::do_mmap()` | fixed/hint/hole 选择、页对齐 |
| `MAP_FIXED_NOREPLACE` 错误 | `VmaSet::has_overlap()` | 目标范围是否与已有 VMA 重叠 |
| munmap 后再次 mmap 失败 | `VmaSet::release_mmap_range()` | holes 是否恢复 |
| mprotect 后权限异常 | `VmaSet::protect_range()` | VMA 权限、PTE 权限、TLB |
| brk 不增长 | `do_sbrk()` | heap 边界、匿名私有映射、ENOMEM |
| mincore 结果异常 | `mincore_range()` | PTE resident 与 file page cache resident |

## 5. CoW 定位

CoW 要核对父子两边：

| 阶段 | 正确状态 |
|------|----------|
| fork 后 | private writable resident 页共享 frame，父子 PTE 均撤销 W |
| 首次写 fault | 进入 `FaultAction::Cow` |
| 独占 frame | 恢复 W，不复制 |
| 共享 frame | 分配新 frame，复制旧页，PTE 改到新 PPN |
| 失败回滚 | `VmPageStore` 和 PTE 恢复旧 frame |

`MAP_SHARED` 不走 CoW。文件 shared 首次写清 W 是为了标脏 PageCache，不是复制私有页。

## 6. OOM 与锁

| 现象 | 检查 |
|------|------|
| frame 不足 | `unallocated_frames()`、OOM recovery、shared/locked 页 |
| heap 不足 | `heap_stats()`、`try_reserve()` |
| mmap 返回 ENOMEM | overcommit、max_map_count、VMA split reserve |
| 回收死锁 | 是否持 inode/VMA/PageCache 锁进入等待或 I/O |
| OOM 后任务未杀 | `pending_oom_kill` 是否在 trap return 安全点处理 |

## 7. 测试映射

| 功能 | 测试 |
|------|------|
| ELF/exec 映射 | busybox、LTP exec |
| brk/malloc | libc malloc、LTP `brk*` |
| mmap/munmap | LTP `mmap*`, `munmap*`, mmapstress |
| mprotect | LTP `mprotect*`，权限 fault |
| fork CoW | fork/clone、copy-on-write 测试 |
| file mmap | iozone、LTP file mmap |
| uaccess | read/write/iovec、bad pointer 用例 |
| mlock/mincore/madvise | LTP mm |
| OOM | `oom_handler` feature、内存压力 |
