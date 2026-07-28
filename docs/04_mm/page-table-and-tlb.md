---
title: "页表抽象与 TLB 约束"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-07-28
tags: [mm, pagetable, tlb, tlb-batch, sv39, loongarch64, smp]
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
| `try_map_no_flush(vpn, ppn, flags)` | 建立映射但不刷新，只允许 `TlbBatch` 调用 |
| `map(vpn, ppn, flags)` | 建立映射，失败时由实现处理 |
| `map_identical(vpn, ppn, flags)` | 建立恒等映射 |
| `unmap(vpn)` | 删除映射 |
| `unmap_no_flush(vpn)` | 删除映射但不刷新，只允许 `TlbBatch` 调用 |
| `translate(vpn)` | VPN 到 PPN |
| `translate_va(va)` | VA 到 PA，保留页内偏移 |
| `block_and_ret_mut(vpn)` | 撤销写权限并返回 PPN，带刷新 |
| `block_and_ret_mut_no_flush(vpn)` | 撤销写权限并返回 PPN，不刷新 |
| `set_ppn_no_flush()` / `set_pte_flags_no_flush()` | 修改 PPN/权限但不刷新，只允许 `TlbBatch` 调用 |
| `set_dirty_bit_no_flush(vpn)` | 统一提交 LA64 软件 dirty fault 对 PTE 的修改 |
| `flush_tlb_page(vpn)` | 刷新本 CPU 的指定虚拟页；架构可保守升级为全量刷新 |
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

## 5. UserMapper、TlbBatch 与 PageMapper

`os/src/mm/user_mapper.rs` 在 `TlbBatch` 上增加用户页权限检查。
`os/src/mm/mapper.rs` 中的 `PageMapper` 仍服务于内核页表，不再是用户 PTE
的写入入口。

```
Vma / page_fault
  └── UserMapper
        └── TlbBatch
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

## 6. TlbBatch 提交责任

页表修改后，硬件可能继续使用旧 TLB 项。B16 将用户 PTE 写入收口为
`TlbBatch`，不再由 VMA/缺页路径各自决定何时刷新。

`TlbPublication` 描述地址空间的发布范围：

| 状态 | 当前语义 |
|------|----------|
| `Unpublished` | 页表尚未被任务激活，batch 提交时无需刷新 |
| `LocalOnly` | 只有一颗 CPU 曾登记缓存该 MM，batch 刷新当前 CPU |
| `Published` | 至少两颗 CPU 曾登记缓存该 MM；B23 接通锁外 shootdown 前，构造 batch 即 fail-stop |

一次提交的固定顺序是：

1. 通过 raw/no-flush 原语修改 PTE，记录受影响 VPN；
2. 若映射指向的 frame 将失去所有权，把 `Arc<FrameTracker>` 移入
   `deferred_frames`；
3. 单一 VPN 请求页级刷新，出现第二个不同 VPN 后升级为本核全量刷新；
4. 刷新完成后才清空 `deferred_frames`；
5. 显式 `commit()` 与 `Drop` 共用同一提交逻辑，`?`/提前返回不会漏 flush。

延迟队列无法扩容时，`defer_frame()` 会先提交当前小批次，再释放当前
frame。这只损失批处理效率，不改变“先失效 TLB，后复用物理页”的安全顺序。

在 fork 路径中，父、子页表各自持有一个 batch：父 batch 批量撤销私有可写页的
W 权限，子 batch 建立共享 PPN 映射。子地址空间尚未发布，可以不刷新；父
batch 必须提交，否则父进程可能继续通过旧 TLB 写共享页，破坏 CoW。

B22 为每个地址空间增加共享 `MmTlbState`。用户 trap-return 在 VM 锁内先把当前 CPU
加入只增不减的 `cached_cpus`，再读取 generation；若 `observed[cpu]` 落后，就在恢复
页表根前清除本地全部用户/non-global 翻译并重查 generation。集合暂不清 bit，因为离开
MM 的 CPU 仍可能缓存旧翻译；最多 8 核的保守额外 IPI 比漏掉目标安全。

B22 还提供独立的 user-TLB request/ack 与锁外全用户失效原语，但 `Published` 仍 fail-stop。
原因是现有 `TlbBatch::commit()` 在外层 VM 锁内运行：若在这里等待远端 ack，目标 CPU
可能正以 IRQ-off page fault 等待同一锁，形成环形等待。B23 必须在 VM 锁内完成 PTE 写入、
generation 推进和目标快照，把 deferred frame 移交给提交对象；释放锁后才等待 ack，全部
完成后释放 frame。激活登记与修改侧快照继续共用 VM 锁，不能仅凭不同原子的
Acquire/Release 假设二者自动串行。

B21 已为共享内核页表的动态映射增加单独的远端协议：公开撤映射先以 no-flush 原语清 PTE、
保留 mapping frame，释放 `KERNEL_SPACE` 锁后执行全 CPU shootdown，收到全部 ack 才释放
frame。它覆盖内核栈和临时 ELF/interpreter 映射，但不应被表述为用户 MM shootdown 完成。

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
self.with_tlb_batch(|vmas, batch| vmas.unmap_all(batch))
    .expect("zombie cleanup failed to clear a resident user PTE");
self.locked_pages.clear();
self.page_table.release_frames();
```

该路径先撤销所有 resident PTE，由 batch 刷新并释放 VMA frame，最后才释放
页表页。等待接口只需要 pid、退出码等元数据，不再需要地址空间。

## 12. 架构相关注意点

| 架构 | 页表/TLB 注意点 |
|------|-----------------|
| rv64 | LocalOnly 单页提交使用 `sfence.vma va, zero`，多页提交和 B22 user-TLB IPI 使用本 hart 全量 `sfence.vma` |
| la64 | ASID 仍由 TCB 持有，页表对象无法安全指定目标 ASID；LocalOnly 提交和 B22 user-TLB IPI 保守执行本核全部 `G=0` 项失效（`invtlb 0x3`） |

上层文档统一称为 `tlb_invalidate` 或 `flush_tlb()`，不把架构指令混入通用 MM 逻辑。

页表和 VMA 是两份必须保持一致的状态。VMA 说明“这段虚拟地址应该如何被访问”，页表说明“硬件当前如何翻译这个 VPN”。缺页、mprotect、munmap、fork CoW 都会同时影响二者中的至少一份：只改 VMA 不改已存在 PTE，会让硬件继续按旧权限运行；只改 PTE 不改 VMA，下一次 fault 或 proc maps/mincore 会看到错误语义。

TLB 是第三份缓存状态。即使页表内存已经改对，CPU 仍可能使用旧 TLB 项；
而 unmap/CoW 如果提前释放 frame，旧 TLB 还会把已复用的物理页当成旧映射访问。
因此用户 PTE 修改必须经过 `TlbBatch`，并把旧 frame 保留到提交之后。

B16 完成 LocalOnly batch；B22 完成 cached CPU/generation 激活侧和全用户 IPI/ack 原语。
PTE 修改侧的 generation 推进、锁外等待与 ack 前 frame 不复用仍属于 B23；MM-owned
ASID/epoch 和 range 优化也尚未完成。在这些门禁通过前，普通用户任务不得跨 CPU 运行。

## 13. 调试核对点

| 现象 | 重点检查 |
|------|----------|
| fork 后父进程写入没有触发 COW | 父 `TlbBatch` 是否记录撤销 W 并成功提交 |
| `mprotect` 后权限仍旧生效 | `protect_range()` 是否经 batch 修改 PTE，本地刷新是否执行 |
| 用户指针明明在 VMA 内仍 EFAULT | fault-in 后 `user_access_ok()` 是否失败 |
| MAP_SHARED 文件写不落到 page cache | 首次读映射是否错误保留 W，绕过 shared write fault |
| unmap 后出现旧页数据或 UAF | 是否先撤销 PTE，再 `defer_frame()`，最后提交 batch |
| zombie 释放后页表泄露 | `unmap_all()` 提交后是否调用 `release_frames()` |
