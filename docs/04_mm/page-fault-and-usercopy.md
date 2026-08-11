---
title: "缺页处理与用户内存 fault-in"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-10
tags: [mm, page-fault, cow, uaccess, mmu-gather]
---

# 缺页处理与用户内存 fault-in

## 1. 入口链路

用户缺页从架构 trap 入口进入，最终到达 `AddressSpace`：

```
arch trap handler
  └── task/process vm
        └── AddressSpace::fault_in_trap_va(addr, access)
              ├── frame_reserve(3)
              └── AddressSpace::do_page_fault(addr, access)
                    ├── VmaSet::find_user_vma_key()
                    ├── VmaSet::expand_growsdown_for_fault()
                    └── page_fault::handle_page_fault()
```

用户内存访问函数也使用同一套缺页处理，但入口是 `uaccess.rs`：

```
copy_from_user / translated_str / UserBuffer copy
  └── copy_user_bytes_progress()
        └── AddressSpace VM lock
              └── fault_in_user_va()
                    ├── resolve_user_va_inner() fast path
                    ├── do_page_fault() when needed
                    └── validate_user_fault_result()
```

两者区别是：trap fault 成功后只要求缺页处理返回物理地址；uaccess fault-in 成功后还会再次检查用户 PTE 权限。

## 2. FaultContext

`os/src/mm/page_fault.rs:10` 定义：

```rust
pub(super) struct FaultContext {
    pub addr: VirtAddr,
    pub vpn: VirtPageNum,
    pub access: FaultAccess,
}
```

`FaultContext::new(addr, access)` 将 fault 地址向下取整为 VPN。`offset_phys(ppn)` 保留原始页内偏移，用于返回准确物理地址。

`FaultContext` 是缺页处理内部的最小上下文：它不保存 task、process、VMA 指针或 errno，只保存地址、页号和访问类型。外层 `AddressSpace::do_page_fault()` 负责找到 VMA，`page_fault.rs` 只围绕单个 VMA 和页表执行修复动作。

## 3. 权限检查

缺页处理第一步是 `check_area_permission(area, ctx)`：

```rust
if area.vm_allows(ctx.access) {
    Ok(())
} else {
    Err(MemoryError::NoPermission)
}
```

`Vma::vm_allows()` 把 `FaultAccess` 转成对应 `MapPermission`：

| FaultAccess | 需要权限 |
|-------------|----------|
| `Load` | `R` |
| `Store` | `W` |
| `Execute` | `X` |

权限不满足时不会进入 lazy alloc、COW 或文件页加载。

## 4. 缺页分类

`FaultAction` 枚举定义在 `page_fault.rs:31`，`PageFaultHandler::handle()` 位于 `page_fault.rs:58`。处理顺序是先 `check_area_permission(area, ctx)?`，再根据 `classify()` 的结果分派到具体动作。

`PageFaultHandler::handle()` 的完整 match 代码在本节后续给出；这里先列出分类结果与触发条件。

`PageFaultHandler::classify()` 位于 `page_fault.rs:102`，根据“页表是否已有映射”和 “VMA 类型/页状态”生成 `FaultAction`：

| 条件 | FaultAction |
|------|-------------|
| PTE 已映射，读或执行 | `MappedRead` |
| PTE 已映射，store 且 VMA 是 shared | `SharedWrite` |
| PTE 已映射，store 且 stale lazy | `StaleLazyPte` |
| PTE 已映射，store 其他情况 | `Cow` |
| 文件映射，store 且 shared | `FileBackedSharedWrite` |
| 文件映射，store 且 private | `FileBackedWrite` |
| 文件映射，load/execute | `FileBackedRead` |
| ELF PT_LOAD 页 `Unallocated` | `ElfLazy` |
| 匿名页 `Unallocated` | `LazyAlloc` |
| 匿名页 `InMemory` 但 PTE 未映射 | `ResidentWithoutPte` |
| 匿名页 `Compressed` | `Decompress`，仅 `oom_handler` |
| 匿名页 `SwappedOut` | `SwapIn`，仅 `oom_handler` |

`ElfLazy` 不能只根据 VMA 类型判断。PTE 不存在时仍须先查 `VmPageState`：
`Unallocated` 才从 ELF 后备装页，`InMemory` 恢复已 resident 页，而
`Compressed/SwappedOut` 继续走原有解压/换入路径。否则 PTE 撤销、压缩或换出后会错误从
文件重建页，覆盖进程已修改的私有内容。

这个分类表是理解 MM 行为的核心：VMA 权限只决定“是否允许”，具体动作取决于 PTE 和 `VmPageStore` 状态。

读 `classify()` 时需要先判断“页表已有映射”这一层。已有 PTE 的 store fault 不代表页面不存在，它通常表示权限位被故意收紧：fork CoW 撤销 W、file shared 首次写需要标脏、或 lazy PTE 元数据与页表短暂不一致。没有 PTE 时才进入 VMA 类型分支，匿名页看 `VmPageState`，文件页看访问类型和 shared/private。

因此同样是 store fault，至少有四种不同含义：

| 场景 | 进入动作 | 结果 |
|------|----------|------|
| 匿名私有页 fork 后首次写 | `Cow` | 复制或独占恢复 W。 |
| 文件 `MAP_SHARED` 页首次写 | `FileBackedSharedWrite` 或 `SharedWrite` | 映射 PageCache 页并标脏。 |
| 匿名 lazy 页首次写 | `LazyAlloc` | 分配清零页并建立 writable PTE。 |
| 只读 VMA 写入 | 权限检查失败 | 返回 `NoPermission`，trap 层注入信号。 |

`PageFaultHandler::handle()` 和 `classify()` 的源码如下：

```rust
impl PageFaultHandler {
    fn handle<T: PageTable>(
        area: &mut Vma,
        mapper: &mut UserMapper<'_, T>,
        ctx: FaultContext,
    ) -> Result<PhysAddr, MemoryError> {
        check_area_permission(area, ctx)?;

        match Self::classify(area, mapper, ctx)? {
            // 匿名页首次访问: 分配一个清零物理页。
            FaultAction::LazyAlloc => {
                map_lazy_zero_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // 文件映射页首次读取/执行: 直接映射文件页缓存。
            FaultAction::FileBackedRead => filemap_read_fault(area, mapper, ctx),
            // 文件映射页首次写入共享映射: 映射 page cache 帧并标脏。
            FaultAction::FileBackedSharedWrite => filemap_shared_write_fault(area, mapper, ctx),
            // 文件映射页首次写入私有映射: 分配私有物理页并从文件填充内容。
            FaultAction::FileBackedWrite => filemap_private_fault(area, mapper, ctx),
            // ELF PT_LOAD 首次访问：组装私有页，冷源页通过 Retry 在 VM 锁外读取。
            FaultAction::ElfLazy => return elf_lazy_fault(area, mapper, ctx),
            // 压缩匿名页再次访问: 解压后恢复页表映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::Decompress => {
                finish_decompress_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // 已换出的匿名页再次访问: 从 swap/zram 换入后恢复映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::SwapIn => {
                finish_swap_in_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
            // MAP_SHARED 写保护 fault: 恢复共享写权限。
            FaultAction::SharedWrite => restore_shared_write(area, mapper, ctx),
            // stale lazy PTE: 页表已有项但元数据仍未分配，先清理再修复。
            FaultAction::StaleLazyPte => repair_stale_lazy_pte(area, mapper, ctx),
            // 私有已映射页写入: 触发 COW。
            FaultAction::Cow => copy_private_page(area, mapper, ctx),
            // 已映射页读取/执行: 直接翻译物理地址。
            FaultAction::MappedRead => translate_mapped_page(mapper, ctx),
            // MAP_SHARED anonymous pages may preallocate shared frames but install
            // user PTEs lazily so mincore can still observe real residency.
            FaultAction::ResidentWithoutPte => {
                map_existing_resident_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn))
            }
        }
    }

    fn classify<T: PageTable>(
        area: &mut Vma,
        mapper: &mut UserMapper<'_, T>,
        ctx: FaultContext,
    ) -> Result<FaultAction, MemoryError> {
        if mapper.is_mapped(ctx.vpn) {
            return Ok(match ctx.access {
                FaultAccess::Load | FaultAccess::Execute => FaultAction::MappedRead,
                FaultAccess::Store if area.vm_mapping() == VmAreaMapping::Shared => {
                    FaultAction::SharedWrite
                }
                FaultAccess::Store if area.vm_is_stale_lazy(ctx.vpn) => FaultAction::StaleLazyPte,
                FaultAccess::Store => FaultAction::Cow,
            });
        }

        match area.vm_kind() {
            VmAreaKind::ElfLazy => match area.vm_page_state(ctx.vpn)? {
                VmPageState::InMemory => Ok(FaultAction::ResidentWithoutPte),
                VmPageState::Unallocated => Ok(FaultAction::ElfLazy),
                #[cfg(feature = "oom_handler")]
                VmPageState::Compressed => Ok(FaultAction::Decompress),
                #[cfg(feature = "oom_handler")]
                VmPageState::SwappedOut => Ok(FaultAction::SwapIn),
            },
            VmAreaKind::FileBacked => Ok(match ctx.access {
                FaultAccess::Store if area.vm_mapping() == VmAreaMapping::Shared => {
                    FaultAction::FileBackedSharedWrite
                }
                FaultAccess::Store => FaultAction::FileBackedWrite,
                FaultAccess::Load | FaultAccess::Execute => FaultAction::FileBackedRead,
            }),
            VmAreaKind::Anonymous => match area.vm_page_state(ctx.vpn)? {
                VmPageState::InMemory => Ok(FaultAction::ResidentWithoutPte),
                VmPageState::Unallocated => Ok(FaultAction::LazyAlloc),
                #[cfg(feature = "oom_handler")]
                VmPageState::Compressed => Ok(FaultAction::Decompress),
                #[cfg(feature = "oom_handler")]
                VmPageState::SwappedOut => Ok(FaultAction::SwapIn),
            },
        }
    }
}
```

这段代码可以直接解释上表的优先级：只要页表已经有 PTE，分类就不会进入文件/ELF/匿名的“未映射”分支；store fault 在已映射 PTE 上优先按 shared、stale lazy、COW 分流。页表没有 PTE 时才看 `VmAreaKind` 和 `VmPageState`。

## 5. 匿名 lazy allocation

匿名未分配页首次访问走 `LazyAlloc`：

```rust
let ppn = area.map_one_zeroed_unchecked(mapper, ctx.vpn)?;
Ok(ctx.offset_phys(ppn))
```

`map_one_zeroed_unchecked()` 会：

1. `frame_alloc()` 分配清零页。
2. 在 `VmPageStore` 中记录 `InMemory(frame)`。
3. 通过 `UserMapper` 安装 PTE，并让当前 `MmuGather` 记录该 VPN。
4. 失败时回滚 `VmPageStore`；成功后由外层 `AddressSpace::write()` 根据 cached
   CPU mask 选择无失效、本地失效或远端 shootdown。

内核默认在 demand page 成功映射后执行有界匿名 fault-around；启动参数
`mango.mm.anon_fault_around=off` 可关闭它以执行严格 A/B。该机制只适用于 private
anonymous `LazyAlloc`，不改变 shared、file-backed、ELF、CoW、swap 或 zram 路径。

方向由 fault 前相邻 PTE 自适应判断：仅前一页已映射时向前，仅后一页已映射时向后；
没有方向证据或两侧都已映射时不预测。每次最多额外映射 2 页，并同时要求候选 VPN 位于
同一 VMA、状态仍为 `Unallocated`、PTE 尚未存在。候选页只能来自 idle 预清零池；池空、
边界、页状态变化或映射失败都会立即停止，不触发同步清零和 OOM recovery。

demand 与 speculative PTE 都通过当前 `UserMapper`/`MmuGather` 发布，因此沿用外层统一
TLB shootdown 契约。`/sys/kernel/stats/pagefault` 的 schema v4 导出 attempt、trigger、
映射页数以及 boundary/state/no-prezero/error 停止原因。

## 6. ResidentWithoutPte

`ResidentWithoutPte` 处理“VMA 已持有页帧，但用户 PTE 尚未安装”的情况。当前典型来源是 writable anonymous `MAP_SHARED`：

1. `do_mmap()` 为所有 VPN 预分配 shared frames。
2. 不安装用户 PTE。
3. 首次访问时缺页分类为 `ResidentWithoutPte`。
4. `map_existing_resident_page()` 将已有 frame 映射到页表。

这样父子进程或多个映射能共享同一 backing frame，同时保留懒 PTE 行为。

## 7. 文件映射读缺页

文件映射读或执行走 `filemap_read_fault()`：

1. 获取 `area.vm_file()`。
2. 根据 `area.vm_file_offset(ctx.vpn)` 计算文件偏移。
3. `check_within_file()` 要求偏移在 round-up 后的文件大小范围内。
4. `inode.ensure_page_cache()` 获取 page cache。
5. `frame_for_read(page_index)` 读取或命中缓存页。
6. 对最后一页 EOF 后半段清零。
7. 如果 VMA 权限含 W，则 PTE 映射权限清 W。
8. `VmPageStore` 记录 page cache frame。
9. 安装用户 PTE。
10. `verify_filemap_fault()` 校验 PTE 与 resident frame 一致。

第 7 步保证 private writable 后续 store 进入 COW，shared writable 后续 store 进入 dirty-mark 路径。

## 8. 文件映射写缺页

文件映射写分两类：

| VMA 类型 | 函数 | 行为 |
|----------|------|------|
| private | `filemap_private_fault()` | 分配私有页，从 page cache 复制内容，EOF 尾部清零 |
| shared | `filemap_shared_write_fault()` | 获取 page cache write frame，标脏，并以 VMA 权限映射 |

private 写缺页不修改 page cache；shared 写缺页必须通过 `frame_for_write()`，使 page cache 进入可写/dirty 状态。

## 9. SharedWrite

当 PTE 已映射、访问是 store、VMA 是 shared 时，走 `restore_shared_write()`。

文件 shared 的额外步骤：

```rust
if area.vm_kind() == VmAreaKind::FileBacked {
    pc.frame_for_write(page_index)?;
}
```

然后调用 `UserMapper::set_user_flags(ctx.vpn, area.vm_perm())` 恢复 W 权限。这样一次读缺页映射的只读 page cache 页在首次 store 时能被正确标脏。

## 10. COW

私有已映射页 store fault 走 `copy_private_page()`，内部调用 `Vma::copy_on_write()`。

流程：

1. `cow_source_frame()` 找到旧页 frame。
2. 如启用 OOM handler，若页被压缩或换出，先恢复成 `InMemory` 并更新 PTE PPN。
3. `Arc::strong_count(&old_frame) <= 2` 时，说明除 VMA 持有和本地临时引用外无其他共享者，直接恢复 W。
4. 否则分配新页。
5. 复制旧页整页内容。
6. 用新 frame 替换 `VmPageStore` 中的旧 frame。
7. 修改 PTE PPN。
8. 设置 PTE flags 为 VMA 权限。
9. 任一步失败都会尝试回滚旧 frame 和旧 PPN。

## 11. StaleLazyPte 修复

`StaleLazyPte` 是防御性路径：页表已有有效 PTE，但 VMA 元数据仍认为该页未分配。处理方式：

1. 记录 warn 日志。
2. `area.clear_stale_pte(mapper, ctx.vpn)` 删除旧 PTE，并记录对应失效。
3. 如果是文件映射，返回 `MemoryError::NotMapped`。
4. 匿名映射重新分配清零页。

该路径避免“页表状态和 VMA 页状态不一致”继续扩散。

## 12. fault_in_user_va 后置校验

`AddressSpace::fault_in_user_va()` 是 uaccess 的核心契约：

```rust
self.do_page_fault(addr, access)
    .and_then(|_| self.validate_user_fault_result(addr, access))
    .map_err(memory_error_to_errno)
```

`validate_user_fault_result()` 会检查：

1. `page_table.translate_va(addr)` 必须成功。
2. 物理地址必须位于真实 DRAM bank，且不能是第 0 页或固件 carveout。
3. Load 对应 `user_access_ok(Read)`。
4. Store 对应 `user_access_ok(Write)`。
5. Execute 要求 PTE valid 且 executable。

因此，缺页处理返回成功但 PTE 权限不满足时，uaccess 仍会返回错误。

源码中的 uaccess-facing contract 是：

```rust
pub fn fault_in_user_va(
    &mut self,
    addr: VirtAddr,
    access: FaultAccess,
) -> Result<PhysAddr, isize> {
    super::frame_reserve(3);
    self.do_page_fault(addr, access)
        .and_then(|_| self.validate_user_fault_result(addr, access))
        .map_err(memory_error_to_errno)
}

pub fn fault_in_trap_va(
    &mut self,
    addr: VirtAddr,
    access: FaultAccess,
) -> Result<PhysAddr, isize> {
    super::frame_reserve(3);
    self.do_page_fault(addr, access)
        .map_err(memory_error_to_errno)
}

fn validate_user_fault_result(
    &self,
    addr: VirtAddr,
    access: FaultAccess,
) -> Result<PhysAddr, MemoryError> {
    let vpn = addr.floor();
    let pa = self
        .page_table
        .translate_va(addr)
        .ok_or(MemoryError::NotMapped)?;

    self.validate_fault_phys_addr(addr, pa)?;

    let ok = match access {
        FaultAccess::Load => self
            .page_table
            .user_access_ok(vpn, UserAccess::Read)
            .unwrap_or(false),
        FaultAccess::Store => self
            .page_table
            .user_access_ok(vpn, UserAccess::Write)
            .unwrap_or(false),
        FaultAccess::Execute => {
            self.page_table.is_valid(vpn).unwrap_or(false)
                && self.page_table.executable(vpn).unwrap_or(false)
        }
    };

    if !ok {
        warn!(
            "[fault_in] user va {:#x} failed post-fault permission check: {:?}",
            addr.0, access
        );
        return Err(MemoryError::NoPermission);
    }

    Ok(pa)
}
```

`fault_in_trap_va()` 只做缺页修复和 errno 转换；`fault_in_user_va()` 额外执行 `validate_user_fault_result()`。这就是 trap fault 与 uaccess fault-in 的核心差异。

## 13. MemoryError 到 errno

`address_space.rs` 末尾的 `memory_error_to_errno()` 负责转换。常见对应关系：

| MemoryError | errno |
|-------------|-------|
| `BadAddress` | `EFAULT` |
| `NoPermission` | `EFAULT` 或上层语义错误 |
| `OutOfMemory` | `ENOMEM` |
| `BeyondEOF` | `SIGBUS` 类路径或 `EFAULT`，取决于调用者 |
| `NotMapped` | `EFAULT` |

trap 路径和 syscall 路径对错误的最终处理不同：trap 中的用户缺页失败通常转成信号；uaccess 中的失败通常直接返回负 errno。

## 14. 用户 copy 与缺页

`uaccess` 不只是查页表。`read(fd, buf, len)` 写用户缓冲区时可以触发匿名页 lazy
allocation 或 CoW；但已有 PTE 满足 U/R/W 权限时，不应把软件 copy 伪装成新的硬件 fault。

B59 后，`fault_in_user_va()` 先无副作用解析当前 PTE：成功就直接返回 PA；只有缺页或写权限
尚未建立才进入 page-fault handler。这样避免重复 CoW/SharedWrite、无效 PTE 修改和多余
TLB shootdown。

固定对象、数组、字符串、连续 buffer 与 iovec 最终都逐页执行：

```text
检查用户范围与当前 token
  -> 取得 AddressSpace VM 锁
       -> resolve 当前 PTE，必要时 fault-in
       -> 验证 U/R/W 与物理范围
       -> 在锁内通过 direct-map raw pointer 复制本页 chunk
  -> 释放 VM 锁
  -> 锁外完成 TLB flush/ack
```

`UserBuffer` 构造时只保存 VA 区间，不保存 PA、frame 或 Rust slice。普通构造器预 fault
完整范围；read/pread 的 `new_writable_prefix()` 只 fault-in 尚不可写的首页，后续页只解析
已有 PTE，并在第一个不可写页前截断本轮 I/O。该优化避免为 EOF 或短读之后的页面制造
lazy allocation、CoW 和 TLB shootdown。任何构造期检查都不能 pin 映射，所以每次实际 copy
仍重新走上述流程。跨页或 scatter copy 的后续页失败时，流式接口返回已完成的前缀；固定
格式对象由 exact wrapper 将短 copy 转为 `EFAULT`。

pipe 是受限例外：ring 自旋锁内不能进入 fault handler，因此它在锁外预 fault，锁内只通过
`resolve_user_va()` 接受仍有效的 PTE。映射变化会立即失败或部分完成，不会在自旋锁内等待。

## 15. 调试核对点

| 现象 | 检查 |
|------|------|
| lazy mmap 首次读失败 | VMA 权限、`VmPageStore::Unallocated`、`frame_alloc()` |
| 文件 mmap 最后一页脏数据 | `zero_tail()` 是否执行 |
| shared 文件写没有写回 | 是否经过 `frame_for_write()` |
| private 文件写污染 page cache | 是否错误走了 shared write path |
| uaccess 返回 EFAULT | token 是否当前任务、post-check 权限是否满足 |
| 栈向下增长失败 | `expand_growsdown_for_fault()` 的距离和 guard gap |
