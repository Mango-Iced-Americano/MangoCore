---
title: "页表抽象与 TLB 约束"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [mm, pagetable, tlb, sv39, loongarch64]
---

# 页表抽象与 TLB 约束

## 1. 设计位置

页表抽象位于 `os/src/mm/page_table.rs`。架构相关实现通过 HAL 导出：

| 架构 | `PageTableImpl` | `KernelPageTableImpl` |
|------|-----------------|-----------------------|
| riscv64 | `Sv39PageTable` | SV39 内核页表实现 |
| loongarch64 | `LAFlexPageTable` | LoongArch64 flexible page table |

MM 上层只依赖 `PageTable` trait，因此 `AddressSpace<T: PageTable>`、`Vma`、`VmaSet` 和缺页处理不需要判断当前架构。

## 2. PageTable trait

`PageTable` trait 定义了 MM 子系统需要的页表能力：

| 方法 | 语义 |
|------|------|
| `try_map(vpn, ppn, flags)` | 建立映射，失败返回 `MemoryError` |
| `map(vpn, ppn, flags)` | 建立映射，失败时由实现处理 |
| `map_identical(vpn, ppn, flags)` | 建立恒等映射 |
| `unmap(vpn)` | 删除映射 |
| `translate(vpn)` | VPN 到 PPN |
| `translate_va(va)` | VA 到 PA，保留页内偏移 |
| `block_and_ret_mut(vpn)` | 撤销写权限并返回 PPN，带刷新 |
| `block_and_ret_mut_no_flush(vpn)` | 撤销写权限并返回 PPN，不刷新 |
| `flush_tlb()` | 刷新当前页表相关 TLB |
| `token()` | 返回架构页表 token |
| `activate()` | 激活页表 |
| `set_ppn(vpn, ppn)` | 修改 PTE 指向的物理页 |
| `set_pte_flags(vpn, flags)` | 修改 PTE flags |
| `user_access_ok(vpn, access)` | 检查用户态读写权限 |
| `release_frames()` | 释放页表自身持有的页表页 |

trait 的存在把 VMA 管理、缺页策略和具体 PTE 编码解耦。

## 3. 访问类型

页表层区分两组访问类型。

`FaultAccess` 表示异常或 fault-in 的触发访问：

| 枚举 | 来源 |
|------|------|
| `Load` | 读缺页、`copy_from_user` |
| `Store` | 写缺页、`copy_to_user`、COW |
| `Execute` | 指令缺页 |

`UserAccess` 表示用户指针翻译后的权限需求：

| 枚举 | 用途 |
|------|------|
| `Read` | 读用户内存 |
| `Write` | 写用户内存 |
| `ReadWrite` | 需要同一对象可读可写，如 `translated_refmut` |

`uaccess.rs` 会把 `UserAccess::Read` 转成 `FaultAccess::Load`，把 `UserAccess::Write` 转成 `FaultAccess::Store`。`ReadWrite` 会先 fault-in load，再 fault-in store。

## 4. PTE flags 的来源

上层权限由 `MapPermission` 表示：

```rust
bitflags! {
    pub struct MapPermission: u8 {
        const R = 1 << 1;
        const W = 1 << 2;
        const X = 1 << 3;
        const U = 1 << 4;
        const G = 1 << 5;
    }
}
```

其中：

| 标志 | 含义 |
|------|------|
| `R` | 可读 |
| `W` | 可写 |
| `X` | 可执行 |
| `U` | 用户态可访问 |
| `G` | 全局映射，内核固定映射使用 |

`MapPermission::from_ph_flags()` 从 ELF program header flags 生成 `R/W/X/U` 权限。

## 5. UserMapper 与 PageMapper

`os/src/mm/mapper.rs` 提供通用 `PageMapper`，`os/src/mm/user_mapper.rs` 在其上增加用户页权限检查。

```
Vma / page_fault
  └── UserMapper
        └── PageMapper
              └── PageTable trait
                    └── Sv39PageTable / LAFlexPageTable
```

`UserMapper::map_user_page()` 要求 flags 包含 `MapPermission::U`：

```rust
fn check_user_flags(flags: MapPermission) -> MmResult<()> {
    if flags.contains(MapPermission::U) {
        Ok(())
    } else {
        Err(MemoryError::NoPermission)
    }
}
```

需要映射 trap context 等非用户页时，代码使用 `map_privileged_user_page()`，它不会强制 `U` 标志。

## 6. TLB 刷新责任

页表修改后，硬件可能继续使用旧 TLB 项。TLB 刷新责任分成两类：

| 方法 | 刷新语义 |
|------|----------|
| `block_and_ret_mut()` | 撤销 W 后由实现刷新 |
| `block_and_ret_mut_no_flush()` | 撤销 W 但不刷新，调用者必须之后刷新 |
| `set_pte_flags()` | 修改 flags，具体实现必须保证 PTE 与 TLB 一致 |
| `set_ppn()` | 修改物理页指向，具体实现必须保证 PTE 与 TLB 一致 |
| `unmap()` | 删除映射后必须保证旧转换不可继续使用 |

在 fork 路径中，`Vma::map_from_existing_page_table()` 为减少刷新次数，使用 `block_and_ret_mut_no_flush()`：

```rust
let ppn = src_page_table.block_and_ret_mut_no_flush(vpn);
parent_tlb_dirty |= ppn.is_some();
if parent_tlb_dirty {
    src_page_table.flush_tlb();
}
```

这说明调用者必须在批量撤销父进程写权限后显式 flush，否则父进程可能继续通过旧 TLB 写共享页，破坏 COW。

## 7. COW 中的页表变化

fork 私有可写 VMA 时：

1. 子进程继承同一个 `Arc<FrameTracker>`。
2. 父进程对应 PTE 的 `W` 被撤销。
3. 子进程映射同一 PPN，权限也不带 `W`。
4. 父页表批量处理后 flush。
5. 父或子首次写入触发 Store fault。
6. `Vma::copy_on_write()` 根据 `Arc::strong_count()` 决定复制或恢复 W。

页表层只提供撤销权限、修改 PPN、修改 flags 的原语；是否复制由 VMA 层根据页帧引用数决定。

## 8. MAP_SHARED 中的页表变化

`MAP_SHARED` 不走私有 COW。实现分两种：

| 类型 | 首次读 | 首次写 |
|------|--------|--------|
| 文件 shared | 读映射 page cache 页，并清 W | `filemap_shared_write_fault()` 获取可写 page cache 页并标脏 |
| 匿名 shared | 可预分配 shared frame，PTE 懒安装 | `ResidentWithoutPte` 直接映射已有 shared frame |

对于文件 shared，即使 VMA 权限包含 W，首次读缺页也会清 W，使首次 store 进入 fault 路径，以便 page cache 正确标脏。

## 9. 用户访问检查

`user_access_ok(vpn, access)` 是非 faulting 检查，主要用于：

| 调用点 | 语义 |
|--------|------|
| `user_accessible_len()` | 只探测现有映射，不触发缺页 |
| `validate_user_fault_result()` | fault-in 后确认权限真的满足要求 |

`uaccess` 的 faulting 翻译不依赖 `user_access_ok()` 直接成功。它会先调用 `fault_in_user_va()`，由缺页处理补齐映射，再做 post-check。

## 10. 页表 token

`token()` 返回架构定义的页表标识。它用于：

1. 激活地址空间。
2. 构造临时页表视图。
3. 用户指针翻译函数确认 token 属于当前任务。

`uaccess.rs` 明确限制 faulting 用户访问只面向当前任务：

```rust
if crate::task::current_user_token() != token {
    return Err(EFAULT);
}
```

这避免内核在缺页时错误地操作非当前进程地址空间。

## 11. release_frames()

`AddressSpace::release_for_zombie()` 会调用：

```rust
self.vmas.clear_no_hole();
self.locked_pages.clear();
self.page_table.release_frames();
```

该路径用于僵尸进程释放地址空间资源。等待接口只需要 pid、退出码等元数据，不再需要页表页和 VMA 页帧。

## 12. 架构相关注意点

| 架构 | 页表/TLB 注意点 |
|------|-----------------|
| rv64 | PTE 修改后依赖 `sfence.vma` 类刷新；用户异常入口会把 page fault 转给 `do_page_fault` |
| la64 | PTE 修改后依赖 LoongArch64 TLB 失效指令；store/page modify fault 成功时可设置 dirty/write 相关状态 |

上层文档统一称为 `tlb_invalidate` 或 `flush_tlb()`，不把架构指令混入通用 MM 逻辑。

页表和 VMA 是两份必须保持一致的状态。VMA 说明“这段虚拟地址应该如何被访问”，页表说明“硬件当前如何翻译这个 VPN”。缺页、mprotect、munmap、fork CoW 都会同时影响二者中的至少一份：只改 VMA 不改已存在 PTE，会让硬件继续按旧权限运行；只改 PTE 不改 VMA，下一次 fault 或 proc maps/mincore 会看到错误语义。

TLB 是第三份缓存状态。即使页表内存已经改对，CPU 仍可能使用旧 TLB 项。因此批量撤销 fork 写权限使用 `block_and_ret_mut_no_flush()` 后必须统一 `flush_tlb()`；单页 `set_ppn/set_pte_flags/unmap` 则依赖页表实现内部刷新。新增页表后端时，TLB 契约要和 PTE 修改 API 一起检查。

## 13. 调试核对点

| 现象 | 重点检查 |
|------|----------|
| fork 后父进程写入没有触发 COW | `block_and_ret_mut_no_flush()` 后是否执行 `flush_tlb()` |
| `mprotect` 后权限仍旧生效 | `set_pte_flags()` 与 TLB 刷新实现 |
| 用户指针明明在 VMA 内仍 EFAULT | fault-in 后 `user_access_ok()` 是否失败 |
| MAP_SHARED 文件写不落到 page cache | 首次读映射是否错误保留 W，绕过 shared write fault |
| zombie 释放后页表泄露 | `release_for_zombie()` 是否调用 `release_frames()` |
