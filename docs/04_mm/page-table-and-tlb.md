---
title: "页表抽象与 TLB 约束"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-07-29
tags: [mm, pagetable, tlb, mmu-gather, sv39, loongarch64, smp]
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
| `try_map_no_flush(vpn, ppn, flags)` | 建立映射但不刷新，只允许 `MmuGather` 调用 |
| `map(vpn, ppn, flags)` | 建立映射，失败时由实现处理 |
| `map_identical(vpn, ppn, flags)` | 建立恒等映射 |
| `unmap(vpn)` | 删除映射 |
| `unmap_no_flush(vpn)` | 删除映射但不刷新，只允许 `MmuGather` 调用 |
| `translate(vpn)` | VPN 到 PPN |
| `translate_va(va)` | VA 到 PA，保留页内偏移 |
| `block_and_ret_mut(vpn)` | 撤销写权限并返回 PPN，带刷新 |
| `block_and_ret_mut_no_flush(vpn)` | 撤销写权限并返回 PPN，不刷新 |
| `set_ppn_no_flush()` / `set_pte_flags_no_flush()` | 修改 PPN/权限但不刷新，只允许 `MmuGather` 调用 |
| `set_dirty_bit_no_flush(vpn)` | 统一提交 LA64 软件 dirty fault 对 PTE 的修改 |
| `flush_tlb_page(vpn)` | 刷新本 CPU 的指定虚拟页；架构可保守升级为全量刷新 |
| `flush_tlb()` | 刷新当前页表相关 TLB |
| `token()` | 返回架构页表 token |
| `activate()` | 激活页表 |
| `set_ppn(vpn, ppn)` | 修改 PTE 指向的物理页 |
| `set_pte_flags(vpn, flags)` | 修改 PTE flags |
| `user_access_ok(vpn, access)` | 检查用户态读写权限 |
| `take_frames()` | 移出页表自身持有的页表页，交给 TLB retirement 延迟释放 |

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

## 5. UserMapper、MmuGather 与 PageMapper

`os/src/mm/user_mapper.rs` 在 `MmuGather` 上增加用户页权限检查。
`os/src/mm/mapper.rs` 中的 `PageMapper` 仍服务于内核页表，不再是用户 PTE
的写入入口。

```
Vma / page_fault
  └── UserMapper
        └── MmuGather
              └── PageTable raw/no-flush methods
                    └── Sv39PageTable / LAFlexPageTable

KernelMapper
  └── PageMapper
        └── PageTable safe methods
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

## 6. 用户页表修改与 TLB 同步

页表修改后，硬件可能继续使用旧 TLB 项。当前实现把职责分成四个边界清晰的
对象，不再使用 `Published` 状态、pending 队列或多层 commit 包装：

| 对象 | 生命周期与责任 |
|------|----------------|
| `AddressSpace` | 共享 MM 的外层对象；持有 VM 锁和长期 `TlbContext` |
| `UserMapper` | 只在 VM 锁内短暂存在；执行 raw/no-flush PTE 写入并立即调用 `record_change()` |
| `MmuGather` | 一次 `AddressSpace::write()` 内唯一的失效范围和退休 frame 所有者 |
| `TlbFlush` | `seal()` 后带出 VM 锁；执行本地/远端失效，收齐 ack 后释放 frame |

调用链固定为：

```text
AddressSpace::write
  -> lock VM
  -> UserMapper 修改 PTE
  -> MmuGather::record_change / retire_frame
  -> MmuGather::seal(&TlbContext)
  -> unlock VM
  -> TlbFlush::execute
```

`MmuGather` 只保留三个概念：失效范围、写操作开始时的 cached CPU mask 快照，
以及一份 `retired_frames`。它把同一 VPN 的多次修改合并为单页失效，把多个 VPN
或页表层级变化合并为全用户失效。没有 PTE 修改时 `seal()` 返回 `None`，因此
只读或未触页的写作用域不会产生 generation、IPI 或 TLB flush。

目标范围不再由额外发布状态表示，而直接从 `TlbContext.cached_cpus` 推导：

| cached CPU mask | 执行方式 |
|-----------------|----------|
| `0` | 页表尚未被任何 CPU 使用；无需失效，解锁后即可释放退休 frame |
| 仅当前 CPU | 按 `FlushRange` 执行页级或本地全用户失效 |
| 含远端 CPU、单一 VPN | RV64 通过同步 SBI RFENCE 执行页级失效；RFENCE 缺失或 LA64 使用全用户 IPI/ack fallback |
| 含远端 CPU、多 VPN/页表层级变化 | 使用 user-TLB request/ack，在目标 CPU 执行全用户失效 |

固定安全顺序是：PTE write → `record_change()` → `retire_frame()` → `seal()` →
释放 VM 锁 → flush/ack → drop frame。退休队列扩容失败时，只有 mask 为 0 或
仅含当前 CPU 才能在锁内证明旧翻译不可访问后同步释放；存在远端观察者时会
故意泄漏 frame 并 fail-stop，绝不让 panic 展开提前复用物理页。

`TlbContext` 保存 MM ID、只增不减的 cached CPU mask、generation 和每 CPU
observed。用户返回前，`activate_on()` 在同一把 VM 锁内先登记 CPU，再比较
generation；若本 CPU 落后，先清除本地用户翻译再使用页表根。修改侧也在这把锁内
读取 mask 并推进 generation，因此激活和修改不会互相漏过。

fork 时父、子分别使用一个 `UserMapper`，修改记录落入各自 `MmuGather`：父侧批量
撤销私有可写页的 W 权限，子侧建立共享 PPN。新子 MM 在包装成 `AddressSpace`
前尚无 CPU 能观察，构造期记录可由 `discard_unpublished()` 清除；父侧记录则由
外层 `write()` 正常同步，否则父 CPU 可能继续用旧权限写共享页。

当前性能策略仍然保守：cached mask 尚未安全清 bit，多页修改和 LA64 远端协议仍按整个
用户地址空间失效。RV64 单页修改已经把 `FlushRange::Page` 直接交给 SBI RFENCE，固件
以物理 hart mask 同步完成 `[va, va + PAGE_SIZE)` 后才允许释放 frame；缺少 RFENCE 时启动
日志会明确说明并退回软件 IPI。LA64 已使用 MM-owned ASID，普通 context switch 不再
固定执行 `invtlb 0x3`；但通用 shootdown 接口尚未携带目标 ASID，单页 PTE 修改仍保守
清除目标 CPU 的全部 non-global 项。已经避免的固定成本还包括：已登记 CPU 的重复
`fetch_or`、同一 VM 写操作的重复 generation/IPI，以及无 PTE 修改时的空 flush。连续
range、LA64 ASID+VA payload 和安全 CPU detach 留给后续工作。

B21 的共享内核页表协议与这里独立：动态内核映射先清 PTE、保留 mapping frame，
释放 `KERNEL_SPACE` 锁后执行全 CPU shootdown，收齐 ack 才释放 frame。

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

## 11. take_frames() 与页表页退休

`AddressSpaceInner::release_for_zombie()` 会在外层 `AddressSpace::write()` 中调用：

```rust
self.with_user_mapper(|vmas, mapper| vmas.unmap_all(mapper))
    .expect("zombie cleanup failed to clear a resident user PTE");
self.locked_pages.clear();
let page_table_frames = self.page_table.take_frames();
self.mmu_gather.record_full_flush();
self.mmu_gather.retire_frames(&self.page_table, page_table_frames);
```

该路径先撤销所有 resident PTE，然后把页表根/中间页也移出所有权。
叶子数据 frame 和页表 frame 由同一个 `MmuGather` 保留，因为远端硬件可能仍在
page walk；外层解锁并收齐目标 CPU 的 ack 后才统一释放。等待接口只需要 pid、
退出码等元数据，不再需要地址空间。

## 12. 架构相关注意点

| 架构 | 页表/TLB 注意点 |
|------|-----------------|
| rv64 | 仅当前 CPU 且只改一个 VPN 时使用 `sfence.vma va, zero`；多页和远端 user-TLB IPI 使用本 hart 全量 `sfence.vma` |
| la64 | ASID 由 `AddressSpace` 的 `TlbContext` 持有；页级入口尚未携带目标 ASID，因此本地与远端 fallback 当前仍保守执行本核全部 `G=0` 项失效（`invtlb 0x3`） |

LA64 把软件 epoch 和硬件 ASID 编码在一个 `asid_context` 中：低 10 位对应
`CSR.ASID[9:0]`，高位只供软件判断编号是否属于当前 epoch。同一 epoch 内的编号单调分配，
MM 销毁不立即归还；耗尽时由一个 leader 先通过既有 user-TLB request/ack 清除全部 online
CPU 的 non-global 项，收到全部 ack 后才推进 epoch 并允许编号复用。等待 rollover 的 CPU
不持 VM 锁，并临时开放本地中断，因此仍能响应 leader 的 TLB IPI。

用户返回路径调用 `ProcessControlBlock::activate_user_vm()`，一次取得同一个
`AddressSpace` 的页表根和 ASID 快照。LA64 `__restore` 只在这对值变化时成对写入
`CSR.PGDL/CSR.ASID`，不再在每次任务切换时全刷 TLB。该组织遵循 LoongArch 官方
ASID/INVTLB 语义以及 Linux LoongArch 的 versioned ASID 做法：失效发生在 ASID 换代，
而不是发生在每次 context switch。

上层文档统一称为 `tlb_invalidate` 或 `flush_tlb()`，不把架构指令混入通用 MM 逻辑。

页表和 VMA 是两份必须保持一致的状态。VMA 说明“这段虚拟地址应该如何被访问”，页表说明“硬件当前如何翻译这个 VPN”。缺页、mprotect、munmap、fork CoW 都会同时影响二者中的至少一份：只改 VMA 不改已存在 PTE，会让硬件继续按旧权限运行；只改 PTE 不改 VMA，下一次 fault 或 proc maps/mincore 会看到错误语义。

TLB 是第三份缓存状态。即使页表内存已经改对，CPU 仍可能使用旧 TLB 项；
而 unmap/CoW 如果提前释放 frame，旧 TLB 还会把已复用的物理页当成旧映射访问。
因此用户 PTE 修改必须经过 `MmuGather`，并把旧 frame 保留到提交之后。

B16 首次收口用户 PTE 写入，B22 完成 cached CPU/generation 激活侧和全用户 IPI/ack
原语；B23 将临时的 batch/pending/commit 原型重构为
`record_change -> seal -> execute`，并完成锁外等待与 ack 前 frame 不复用；B24 接通
RV64 单页 RFENCE；B25 完成 LA64 MM-owned ASID 与全 CPU flush-before-reuse epoch
协议。连续 range、LA64 精确 ASID+VA shootdown、安全 CPU detach 和普通用户迁移仍未完成。

## 13. 调试核对点

| 现象 | 重点检查 |
|------|----------|
| fork 后父进程写入没有触发 COW | 父 `MmuGather` 是否记录撤销 W 并成功提交 |
| `mprotect` 后权限仍旧生效 | `protect_range()` 是否经 `UserMapper` 修改 PTE，`MmuGather` 是否记录该 VPN |
| 用户指针明明在 VMA 内仍 EFAULT | fault-in 后 `user_access_ok()` 是否失败 |
| MAP_SHARED 文件写不落到 page cache | 首次读映射是否错误保留 W，绕过 shared write fault |
| unmap 后出现旧页数据或 UAF | 是否先撤销 PTE，再 `retire_frame()`，最后执行 `TlbFlush` |
| zombie 释放后页表泄露 | `unmap_all()` 后是否将 `take_frames()` 结果交给同一轮 retirement |
