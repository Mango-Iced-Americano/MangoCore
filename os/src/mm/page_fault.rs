//! 用户页缺页处理状态机。
//!
//! `AddressSpaceInner::do_page_fault` 先定位 VMA，再把 fault 交给本模块分类并修复：
//! 匿名懒分配、文件映射读取/写入、共享写恢复、CoW、压缩页解压和 swap-in 都在这里汇聚。
//!
//! # TLB
//!
//! 本模块只通过 `Vma`/`UserMapper` 修改 PTE；`UserMapper` 同步记录到
//! `MmuGather`，由外层地址空间在解锁后执行 TLB 失效。不得在此绕过该边界。

use super::filemap::{
    elf_lazy_fault, filemap_private_fault, filemap_read_fault, filemap_shared_write_fault,
};
use super::user_mapper::UserMapper;
use super::vma::Vma;
use super::vma::{VmAreaKind, VmAreaMapping, VmPageState};
use super::{FaultAccess, FaultOutcome, MemoryError, PageTable, PhysAddr, VirtAddr, VirtPageNum};
use crate::task::perf::AnonFaultAroundStopReason;
use crate::utils::error::SyscallErr;
use core::sync::atomic::{AtomicU8, Ordering};
use log::{error, warn};

const ANON_FAULT_AROUND_UNINITIALIZED: u8 = 0;
const ANON_FAULT_AROUND_ENABLED: u8 = 1;
const ANON_FAULT_AROUND_DISABLED: u8 = 2;
const ANON_FAULT_AROUND_PAGES: usize = 2;

static ANON_FAULT_AROUND_POLICY: AtomicU8 = AtomicU8::new(ANON_FAULT_AROUND_UNINITIALIZED);

fn parse_anon_fault_around_enabled() -> bool {
    crate::bootargs::get_cmdline()
        .split_whitespace()
        .find_map(|token| token.strip_prefix("mango.mm.anon_fault_around="))
        .map(|value| !matches!(value, "off" | "0" | "false" | "no"))
        .unwrap_or(true)
}

pub(super) fn anon_fault_around_enabled() -> bool {
    let policy = ANON_FAULT_AROUND_POLICY.load(Ordering::Acquire);
    if policy != ANON_FAULT_AROUND_UNINITIALIZED {
        return policy == ANON_FAULT_AROUND_ENABLED;
    }
    let parsed = if parse_anon_fault_around_enabled() {
        ANON_FAULT_AROUND_ENABLED
    } else {
        ANON_FAULT_AROUND_DISABLED
    };
    match ANON_FAULT_AROUND_POLICY.compare_exchange(
        ANON_FAULT_AROUND_UNINITIALIZED,
        parsed,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => parsed == ANON_FAULT_AROUND_ENABLED,
        Err(installed) => installed == ANON_FAULT_AROUND_ENABLED,
    }
}

#[derive(Clone, Copy)]
enum AnonFaultAroundDirection {
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 一次缺页的规范化上下文。
pub(super) struct FaultContext {
    /// 原始 fault 虚拟地址，保留页内偏移。
    pub addr: VirtAddr,
    /// `addr` 所在虚拟页。
    pub vpn: VirtPageNum,
    /// 触发 fault 的访问类型。
    pub access: FaultAccess,
}

impl FaultContext {
    /// 从 fault 地址和访问类型构造上下文。
    pub fn new(addr: VirtAddr, access: FaultAccess) -> Self {
        Self {
            addr,
            vpn: addr.floor(),
            access,
        }
    }

    /// 把页级物理页号加上 fault 地址的页内偏移。
    pub(super) fn offset_phys(self, ppn: super::PhysPageNum) -> PhysAddr {
        ppn.offset(self.addr.page_offset())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 缺页处理器对当前 fault 选择的修复动作。
pub(super) enum FaultAction {
    /// 匿名页首次访问，分配清零物理页。
    LazyAlloc,
    /// 文件映射首次读/执行，映射 page cache 只读页。
    FileBackedRead,
    /// 文件私有映射首次写，复制 page cache 内容到私有页。
    FileBackedWrite,
    /// 文件共享映射首次写，取得可写 page cache 页并恢复 W。
    FileBackedSharedWrite,
    /// ELF PT_LOAD page assembled into a private frame on first access.
    ElfLazy,
    #[cfg(feature = "oom_handler")]
    /// 压缩匿名页再次访问，需要从 zram 解压。
    Decompress,
    #[cfg(feature = "oom_handler")]
    /// 已换出匿名页再次访问，需要从 swap/zram 换入。
    SwapIn,
    /// 已映射共享页写 fault，仅恢复共享写权限。
    SharedWrite,
    /// 页表保留了懒分配残留 PTE，需要先清理再重新分配。
    StaleLazyPte,
    /// 私有映射写 fault，执行 copy-on-write。
    Cow,
    /// 已映射页的读/执行 fault，只需返回翻译结果。
    MappedRead,
    /// VMA 已有 resident frame，但用户 PTE 尚未安装。
    ResidentWithoutPte,
}

struct PageFaultHandler;

/// 处理一个已定位到 VMA 的用户页 fault。
///
/// # Errors
///
/// 权限不匹配返回 `MemoryError::NoPermission`；后端文件、OOM、swap/zram 或 PTE
/// 修复失败时透传对应 `MemoryError`。
pub(super) fn handle_page_fault<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    PageFaultHandler::handle(area, mapper, ctx)
}

impl PageFaultHandler {
    fn handle<T: PageTable>(
        area: &mut Vma,
        mapper: &mut UserMapper<'_, T>,
        ctx: FaultContext,
    ) -> FaultOutcome {
        crate::task::perf::record_pagefault_attempt(ctx.access);
        if let Err(error) = check_area_permission(area, ctx) {
            crate::task::perf::record_pagefault_outcome(2);
            return FaultOutcome::Error(error);
        }

        let classify_start = crate::task::perf::perf_memory_io_time_now();
        let action = match Self::classify(area, mapper, ctx) {
            Ok(action) => action,
            Err(error) => {
                crate::task::perf::record_pagefault_stage(
                    1,
                    crate::task::perf::perf_memory_io_time_now().wrapping_sub(classify_start),
                );
                crate::task::perf::record_pagefault_outcome(2);
                return FaultOutcome::Error(error);
            }
        };
        crate::task::perf::record_pagefault_stage(
            1,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(classify_start),
        );
        let action_tag = match action {
            FaultAction::LazyAlloc => 0usize,
            FaultAction::FileBackedRead => 1,
            FaultAction::FileBackedSharedWrite => 2,
            FaultAction::FileBackedWrite => 3,
            FaultAction::SharedWrite => 4,
            FaultAction::Cow => 5,
            FaultAction::ElfLazy => 6,
            #[cfg(feature = "oom_handler")]
            FaultAction::Decompress => 7,
            #[cfg(feature = "oom_handler")]
            FaultAction::SwapIn => 8,
            FaultAction::StaleLazyPte => 9,
            FaultAction::MappedRead => 10,
            FaultAction::ResidentWithoutPte => 11,
        };
        let _pf_start = crate::task::perf::perf_memory_io_time_now();
        let outcome = match action {
            // 匿名页首次访问：分配清零物理页并安装用户 PTE。
            FaultAction::LazyAlloc => {
                crate::task::perf::record_anon_fault_locality(ctx.vpn.0);
                result_to_outcome(
                    map_lazy_zero_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn)),
                )
            }
            // 文件映射页首次读取/执行：直接映射文件页缓存。
            FaultAction::FileBackedRead => filemap_read_fault(area, mapper, ctx),
            // 文件映射页首次写入共享映射：映射 page cache 帧并标脏。
            FaultAction::FileBackedSharedWrite => filemap_shared_write_fault(area, mapper, ctx),
            // 文件映射页首次写入私有映射：分配私有物理页并从文件填充内容。
            FaultAction::FileBackedWrite => filemap_private_fault(area, mapper, ctx),
            FaultAction::ElfLazy => elf_lazy_fault(area, mapper, ctx),
            // 压缩匿名页再次访问：解压后恢复页表映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::Decompress => result_to_outcome(
                finish_decompress_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn)),
            ),
            // 已换出的匿名页再次访问：从 swap/zram 换入后恢复映射。
            #[cfg(feature = "oom_handler")]
            FaultAction::SwapIn => result_to_outcome(
                finish_swap_in_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn)),
            ),
            // MAP_SHARED 写保护 fault：恢复共享写权限。
            FaultAction::SharedWrite => restore_shared_write(area, mapper, ctx),
            // Stale lazy PTE：页表已有项但元数据仍未分配，先清理再修复。
            FaultAction::StaleLazyPte => {
                result_to_outcome(repair_stale_lazy_pte(area, mapper, ctx))
            }
            // 私有已映射页写入：触发 COW。
            FaultAction::Cow => result_to_outcome(copy_private_page(area, mapper, ctx)),
            // 已映射页读取/执行：直接翻译物理地址。
            FaultAction::MappedRead => result_to_outcome(translate_mapped_page(mapper, ctx)),
            // MAP_SHARED 匿名页可以预分配共享 frame，但延迟安装用户 PTE；
            // 这样 `mincore` 仍能观察到未访问页未 present。
            FaultAction::ResidentWithoutPte => result_to_outcome(
                map_existing_resident_page(area, mapper, ctx).map(|ppn| ctx.offset_phys(ppn)),
            ),
        };
        let elapsed = crate::task::perf::perf_memory_io_time_now().wrapping_sub(_pf_start);
        crate::task::perf::record_pagefault_action(action_tag, elapsed);
        crate::task::perf::record_pagefault_outcome(match &outcome {
            FaultOutcome::Completed(_) => 0,
            FaultOutcome::Retry(_) => 1,
            FaultOutcome::Error(_) => 2,
        });
        outcome
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

#[inline(always)]
fn result_to_outcome(result: Result<PhysAddr, MemoryError>) -> FaultOutcome {
    match result {
        Ok(pa) => FaultOutcome::Completed(pa),
        Err(error) => FaultOutcome::Error(error),
    }
}

fn check_area_permission(area: &Vma, ctx: FaultContext) -> Result<(), MemoryError> {
    if area.vm_allows(ctx.access) {
        Ok(())
    } else {
        error!(
            "[do_page_fault] addr: {:?}, access: {:?}, result: no permission",
            ctx.addr, ctx.access
        );
        Err(MemoryError::NoPermission)
    }
}

fn map_existing_resident_page<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    area.map_existing_in_memory(mapper, ctx.vpn)
}

fn map_lazy_zero_page<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let direction = anon_fault_around_direction(area, mapper, ctx.vpn);
    let ppn = area.map_one_zeroed_unchecked(mapper, ctx.vpn)?;
    populate_anon_fault_around(area, mapper, ctx.vpn, direction);
    Ok(ppn)
}

fn anon_fault_around_direction<T: PageTable>(
    area: &Vma,
    mapper: &UserMapper<'_, T>,
    vpn: VirtPageNum,
) -> Option<AnonFaultAroundDirection> {
    if !anon_fault_around_enabled() || area.vm_mapping() != VmAreaMapping::Private {
        return None;
    }
    let previous_mapped = vpn.0 > area.vm_start().0 && mapper.is_mapped(VirtPageNum(vpn.0 - 1));
    let next_mapped = vpn
        .0
        .checked_add(1)
        .map(VirtPageNum)
        .is_some_and(|next| next < area.vm_end() && mapper.is_mapped(next));
    match (previous_mapped, next_mapped) {
        (true, false) => Some(AnonFaultAroundDirection::Forward),
        (false, true) => Some(AnonFaultAroundDirection::Backward),
        _ => None,
    }
}

fn populate_anon_fault_around<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    fault_vpn: VirtPageNum,
    direction: Option<AnonFaultAroundDirection>,
) {
    crate::task::perf::record_anon_fault_around_attempt(direction.is_some());
    let Some(direction) = direction else {
        return;
    };

    for distance in 1..=ANON_FAULT_AROUND_PAGES {
        let candidate = match direction {
            AnonFaultAroundDirection::Forward => fault_vpn.0.checked_add(distance),
            AnonFaultAroundDirection::Backward => fault_vpn.0.checked_sub(distance),
        }
        .map(VirtPageNum);
        let Some(candidate) = candidate else {
            crate::task::perf::record_anon_fault_around_stop(AnonFaultAroundStopReason::Boundary);
            break;
        };
        if !area.vm_contains(candidate) {
            crate::task::perf::record_anon_fault_around_stop(AnonFaultAroundStopReason::Boundary);
            break;
        }
        if mapper.is_mapped(candidate)
            || !matches!(area.vm_page_state(candidate), Ok(VmPageState::Unallocated))
        {
            crate::task::perf::record_anon_fault_around_stop(AnonFaultAroundStopReason::PageState);
            break;
        }
        match area.try_map_one_prezeroed_unchecked(mapper, candidate) {
            Ok(Some(_)) => crate::task::perf::record_anon_fault_around_page(),
            Ok(None) => {
                crate::task::perf::record_anon_fault_around_stop(
                    AnonFaultAroundStopReason::NoPrezeroedPage,
                );
                break;
            }
            Err(_) => {
                crate::task::perf::record_anon_fault_around_stop(
                    AnonFaultAroundStopReason::MappingError,
                );
                break;
            }
        }
    }
}

#[cfg(feature = "oom_handler")]
fn finish_decompress_page<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let ppn = area.vm_decompress_page(ctx.vpn)?;
    mapper.map_user_page(ctx.vpn, ppn, area.vm_perm())?;
    area.vm_record_resident_page::<T>(ctx.vpn)?;
    area.vm_dec_compressed();
    Ok(ppn)
}

#[cfg(feature = "oom_handler")]
fn finish_swap_in_page<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<super::PhysPageNum, MemoryError> {
    let ppn = area.vm_swap_in_page(ctx.vpn)?;
    mapper.map_user_page(ctx.vpn, ppn, area.vm_perm())?;
    area.vm_record_resident_page::<T>(ctx.vpn)?;
    area.vm_dec_swapped();
    Ok(ppn)
}

fn restore_shared_write<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    // 文件共享页恢复 W 之前先进入 page cache 写路径，确保 dirty 状态不会丢失。
    if area.vm_kind() == VmAreaKind::FileBacked {
        if let (Some(inode), Ok(file_offset)) = (area.vm_file(), area.vm_file_offset(ctx.vpn)) {
            if let Some(pc) = inode.ensure_page_cache() {
                let page_index = file_offset >> crate::config::PAGE_SIZE_BITS;
                match pc.try_frame_for_write(page_index) {
                    Ok(_) => {}
                    Err(crate::fs::PageCacheFault::Retry(wait)) => {
                        return FaultOutcome::Retry(wait)
                    }
                    Err(crate::fs::PageCacheFault::Error(error)) => {
                        return FaultOutcome::Error(match error {
                            SyscallErr::ENOMEM => MemoryError::OutOfMemory,
                            _ => MemoryError::BackingStoreFailure,
                        })
                    }
                }
            }
        }
    }
    if let Err(error) = mapper.set_user_flags(ctx.vpn, area.vm_perm()) {
        return FaultOutcome::Error(error);
    }
    match mapper.translate(ctx.vpn) {
        Some(ppn) => FaultOutcome::Completed(ctx.offset_phys(ppn)),
        None => FaultOutcome::Error(MemoryError::NotMapped),
    }
}

fn repair_stale_lazy_pte<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    warn!(
        "[do_page_fault] clear stale lazy pte: addr={:?}, vpn={:?}, area={:?}",
        ctx.addr, ctx.vpn, area
    );
    area.clear_stale_pte(mapper, ctx.vpn);

    if area.vm_kind() == VmAreaKind::FileBacked {
        return Err(MemoryError::NotMapped);
    }

    let allocated_ppn = area.map_one_zeroed_unchecked(mapper, ctx.vpn)?;
    Ok(ctx.offset_phys(allocated_ppn))
}

fn copy_private_page<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let allocated_ppn = area.copy_on_write(mapper, ctx.vpn)?;
    Ok(ctx.offset_phys(allocated_ppn))
}

fn translate_mapped_page<T: PageTable>(
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let ppn = mapper.translate(ctx.vpn).ok_or(MemoryError::NotMapped)?;
    Ok(ctx.offset_phys(ppn))
}
