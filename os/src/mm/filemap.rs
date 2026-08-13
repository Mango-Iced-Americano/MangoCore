//! 文件映射 VMA 的缺页填充路径。
//!
//! 本模块把 `MAP_PRIVATE`/`MAP_SHARED` 文件映射的首次访问转成 page cache
//! 读写，并在最后校验 VMA resident frame 与用户 PTE 是否一致。
//!
//! # Semantics
//!
//! 读取最后一个不完整文件页时会把 EOF 之后的页尾清零；超过文件大小取整到页的
//! fault 返回 `MemoryError::BeyondEOF`。共享写 fault 会先通过 page cache 取得可写页，
//! 再恢复用户 PTE。

use super::page_fault::FaultContext;
use super::user_mapper::UserMapper;
use super::vma::{VmPageState, Vma};
use super::{MapPermission, MemoryError, PageTable, PhysAddr, PhysPageNum};
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::fs::vfs::IndexNode;
use crate::fs::{PageCache, PageCacheFault, MAX_DEMAND_READ_PAGES};
use crate::mm::FaultOutcome;
use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// One validated PT_LOAD segment, in program-header order.
#[derive(Clone, Copy, Debug)]
pub(super) struct ElfLoadSegment {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) file_offset: usize,
    pub(super) filesz: usize,
    pub(super) map_perm: MapPermission,
}

/// Immutable backing shared by every VMA of one lazily loaded ELF image.
///
/// The segment vector preserves program-header order. A fault starts with a
/// zero page and overlays every intersecting file range in that order, which
/// matches the eager loader for overlapping PT_LOAD pages while naturally
/// retaining zero-filled BSS and page tails.
pub(super) struct ElfLazyBacking {
    cache: Arc<PageCache>,
    _executable: crate::task::ExecutableMappingGuard,
    segments: Vec<ElfLoadSegment>,
}

impl ElfLazyBacking {
    pub(super) fn new(
        cache: Arc<PageCache>,
        inode: Arc<dyn IndexNode>,
        segments: Vec<ElfLoadSegment>,
    ) -> Self {
        Self {
            cache,
            _executable: crate::task::ExecutableMappingGuard::new(inode),
            segments,
        }
    }

    fn try_fill_page(
        self: &Arc<Self>,
        vpn: super::VirtPageNum,
        dst: &mut [u8],
    ) -> Result<(), PageCacheFault> {
        if dst.len() != PAGE_SIZE {
            return Err(PageCacheFault::Error(SyscallErr::EINVAL));
        }
        let page_start = super::VirtAddr::from(vpn).0;
        let page_end = page_start
            .checked_add(PAGE_SIZE)
            .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
        dst.fill(0);

        for segment in &self.segments {
            let file_virtual_end = segment
                .start
                .checked_add(segment.filesz)
                .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
            let copy_start = page_start.max(segment.start);
            let copy_end = page_end.min(file_virtual_end);
            if copy_start >= copy_end {
                continue;
            }

            let mut virtual_cursor = copy_start;
            let mut file_cursor = segment
                .file_offset
                .checked_add(copy_start - segment.start)
                .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
            while virtual_cursor < copy_end {
                let page_index = file_cursor >> PAGE_SIZE_BITS;
                let page_offset = file_cursor & (PAGE_SIZE - 1);
                let copy_len = (copy_end - virtual_cursor).min(PAGE_SIZE - page_offset);
                let dst_offset = virtual_cursor - page_start;
                let dst_end = dst_offset
                    .checked_add(copy_len)
                    .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
                self.cache.try_copy_resident_range(
                    page_index,
                    page_offset,
                    &mut dst[dst_offset..dst_end],
                )?;
                virtual_cursor = virtual_cursor
                    .checked_add(copy_len)
                    .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
                file_cursor = file_cursor
                    .checked_add(copy_len)
                    .ok_or(PageCacheFault::Error(SyscallErr::EIO))?;
            }
        }
        Ok(())
    }
}

fn round_up_page(size: usize) -> usize {
    size.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Bound a forward filemap fault-around window by both the current VMA and
/// the authoritative file size. This prevents a short/random mapping from
/// admitting pages that it can never address.
fn bounded_fault_around_pages(
    area: &Vma,
    ctx: FaultContext,
    file_offset: usize,
    file_size: usize,
) -> usize {
    let vma_pages = area.vm_end().0.saturating_sub(ctx.vpn.0);
    let file_pages = file_size.saturating_sub(file_offset).div_ceil(PAGE_SIZE);
    vma_pages.min(file_pages).min(MAX_DEMAND_READ_PAGES).max(1)
}

fn check_within_file(inode: &dyn IndexNode, file_offset: usize) -> Result<usize, MemoryError> {
    let file_size = inode
        .metadata()
        .map(|m| m.size.max(0) as usize)
        .map_err(|_| MemoryError::BackingStoreFailure)?;
    if file_offset >= round_up_page(file_size) {
        return Err(MemoryError::BeyondEOF);
    }
    Ok(file_size)
}

fn map_pc_error(e: SyscallErr) -> MemoryError {
    match e {
        SyscallErr::ENOMEM => MemoryError::OutOfMemory,
        SyscallErr::EIO => MemoryError::BackingStoreFailure,
        _ => MemoryError::BackingStoreFailure,
    }
}

/// Install read-only PTEs for the contiguous, already-resident tail of a
/// filemap read-ahead window. The demand page has already succeeded before
/// this helper runs, so every speculative failure is advisory: no backend I/O
/// is started under the VM lock and the original fault remains successful.
fn map_resident_filemap_tail<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    pc: &Arc<PageCache>,
    first_vpn: super::VirtPageNum,
    first_page_index: usize,
    pages: usize,
    file_size: usize,
    map_perm: MapPermission,
) {
    if pages <= 1 {
        return;
    }

    let mut examined = 0usize;
    let mut mapped = 0usize;
    let mut not_ready = false;
    let mut state_conflicts = 0usize;
    let mut cache_errors = 0usize;

    for offset in 1..pages {
        let Some(vpn_value) = first_vpn.0.checked_add(offset) else {
            break;
        };
        let Some(page_index) = first_page_index.checked_add(offset) else {
            break;
        };
        let vpn = super::VirtPageNum(vpn_value);
        examined = examined.saturating_add(1);

        // A preceding fault or a non-resident VM state owns this slot. Skip it
        // without disturbing that state, but keep scanning the bounded window.
        if mapper.is_mapped(vpn) || !matches!(area.vm_page_state(vpn), Ok(VmPageState::Unallocated))
        {
            state_conflicts = state_conflicts.saturating_add(1);
            continue;
        }

        let frame = match pc.try_resident_frame_for_filemap_map(page_index, file_size) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                not_ready = true;
                break;
            }
            Err(_) => {
                cache_errors = cache_errors.saturating_add(1);
                break;
            }
        };
        let ppn = frame.ppn;
        if area.inner.alloc_in_memory(vpn, frame).is_err() {
            state_conflicts = state_conflicts.saturating_add(1);
            continue;
        }
        if mapper.map_user_page(vpn, ppn, map_perm).is_err() {
            area.inner.remove_in_memory(&vpn);
            cache_errors = cache_errors.saturating_add(1);
            break;
        }
        pc.map_page(page_index);
        mapped = mapped.saturating_add(1);
    }

    crate::task::perf::record_filemap_pte_around(
        examined,
        mapped,
        not_ready,
        state_conflicts,
        cache_errors,
    );
    crate::task::perf::FILEMAP_FAULT_FRAMES
        .fetch_add(mapped, core::sync::atomic::Ordering::Relaxed);
}

/// Populate one private ELF page on first access.
///
/// PageCache admission never performs backend I/O while the VM lock is held.
/// A cold source page rolls back the unpublished target frame and returns a
/// Retry token; the token loads that source after the caller releases VM.
pub(super) fn elf_lazy_fault<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    let backing = match area.vm_elf_backing() {
        Some(backing) => backing,
        None => return FaultOutcome::Error(MemoryError::NotMapped),
    };
    // Safety: `try_fill_page` first clears the complete destination and then
    // overlays every intersecting PT_LOAD range. The frame has no PTE until
    // that full initialization succeeds; all failure paths remove it below.
    let target_ppn = match unsafe { area.alloc_one_uninit_unmapped(ctx.vpn) } {
        Ok(ppn) => ppn,
        Err(error) => return FaultOutcome::Error(error),
    };
    let fill_result = unsafe {
        // Safety: the target frame is owned by this VMA but has no PTE yet;
        // the address-space write lock makes this fault its only observer.
        target_ppn.with_bytes_mut(|dst| backing.try_fill_page(ctx.vpn, dst))
    };
    match fill_result {
        Ok(()) => {}
        Err(PageCacheFault::Retry(wait)) => {
            area.remove_unmapped_frame(ctx.vpn);
            return FaultOutcome::Retry(wait);
        }
        Err(PageCacheFault::Error(error)) => {
            area.remove_unmapped_frame(ctx.vpn);
            return FaultOutcome::Error(map_pc_error(error));
        }
    }
    if let Err(error) = area.map_existing_in_memory(mapper, ctx.vpn) {
        area.remove_unmapped_frame(ctx.vpn);
        return FaultOutcome::Error(error);
    }
    match verify_filemap_fault(area, mapper, ctx, target_ppn) {
        Ok(pa) => FaultOutcome::Completed(pa),
        Err(error) => FaultOutcome::Error(error),
    }
}

fn verify_filemap_fault<T: PageTable>(
    area: &Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
    expected_ppn: PhysPageNum,
) -> Result<PhysAddr, MemoryError> {
    if area.inner.get_in_memory(&ctx.vpn).is_none() {
        log::warn!(
            "[filemap] fault succeeded without resident frame: vpn={:?}",
            ctx.vpn
        );
        return Err(MemoryError::NotMapped);
    }

    let mapped_ppn = mapper.translate(ctx.vpn).ok_or(MemoryError::NotMapped)?;
    if mapped_ppn != expected_ppn {
        log::warn!(
            "[filemap] pte/frame mismatch: vpn={:?}, pte={:?}, expected={:?}",
            ctx.vpn,
            mapped_ppn,
            expected_ppn
        );
        return Err(MemoryError::BackingStoreFailure);
    }

    Ok(ctx.offset_phys(mapped_ppn))
}

/// 处理 `MAP_PRIVATE` 文件页的写 fault。
///
/// # Semantics
///
/// 分配一页新的私有物理页，从 page cache 拷贝文件内容，并清零 EOF 之后的页尾。
/// 该页后续按匿名私有页处理，不再共享 page cache 帧。
pub(super) fn filemap_private_fault<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    crate::task::perf::record_filemap_private_fault();
    let _pf_start = crate::task::perf::perf_time_now();
    let inode = match area.vm_file() {
        Some(inode) => inode,
        None => return FaultOutcome::Error(MemoryError::NotMapped),
    };
    let file_offset = match area.vm_file_offset(ctx.vpn) {
        Ok(offset) => offset,
        Err(error) => return FaultOutcome::Error(error),
    };
    let file_size = match check_within_file(inode.as_ref(), file_offset) {
        Ok(size) => size,
        Err(error) => return FaultOutcome::Error(error),
    };

    let pc = match inode.ensure_page_cache() {
        Some(pc) => pc,
        None => return FaultOutcome::Error(MemoryError::BackingStoreFailure),
    };
    let page_index = file_offset >> PAGE_SIZE_BITS;
    crate::task::perf::FILEMAP_FAULT_FRAMES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // 先填充未发布页，再安装用户 PTE。若先 map 再 copy，同一 MM 的其它 CPU
    // 可能在内核写入期间访问该页，既暴露半成品数据也破坏 Rust 独占性。
    // Safety: `try_copy_page_for_private` copies a complete cache page before
    // applying the authoritative EOF tail zero. The target is unpublished and
    // every retry/error path removes it before returning.
    let allocated_ppn = match unsafe { area.alloc_one_uninit_unmapped(ctx.vpn) } {
        Ok(ppn) => ppn,
        Err(error) => return FaultOutcome::Error(error),
    };
    let _copy_start = crate::task::perf::perf_time_now();
    let copy_result = unsafe {
        // Safety: frame 已登记在当前 VMA，但尚未安装 PTE；地址空间写锁保证
        // 没有其它 fault 路径取得它，因此本路径独占目标页。
        allocated_ppn.with_bytes_mut(|dst| pc.try_copy_page_for_private(page_index, dst, file_size))
    };
    match copy_result {
        Ok(()) => {}
        Err(PageCacheFault::Retry(wait)) => {
            area.remove_unmapped_frame(ctx.vpn);
            crate::task::perf::record_filemap_not_ready_retry();
            return FaultOutcome::Retry(wait);
        }
        Err(PageCacheFault::Error(error)) => {
            area.remove_unmapped_frame(ctx.vpn);
            return FaultOutcome::Error(map_pc_error(error));
        }
    }
    if let Err(error) = area.map_existing_in_memory(mapper, ctx.vpn) {
        area.remove_unmapped_frame(ctx.vpn);
        return FaultOutcome::Error(error);
    }
    let copy_ticks = crate::task::perf::perf_time_now().wrapping_sub(_copy_start);
    crate::task::perf::FILEMAP_PRIVATE_COPY_TICKS
        .fetch_add(copy_ticks, core::sync::atomic::Ordering::Relaxed);
    crate::task::perf::FILEMAP_FAULT_TICKS.fetch_add(
        crate::task::perf::perf_time_now().wrapping_sub(_pf_start),
        core::sync::atomic::Ordering::Relaxed,
    );
    match verify_filemap_fault(area, mapper, ctx, allocated_ppn) {
        Ok(pa) => FaultOutcome::Completed(pa),
        Err(error) => FaultOutcome::Error(error),
    }
}

/// 处理文件映射页的读/执行 fault。
///
/// # Semantics
///
/// 直接映射 page cache 中的物理页。若 VMA 可写，首次映射会清除 W 位，使后续写入
/// 重新进入 fault 路径并区分 CoW 或共享脏页语义。
pub(super) fn filemap_read_fault<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    crate::task::perf::record_filemap_read_fault();
    let _pf_start = crate::task::perf::perf_time_now();
    let inode = match area.vm_file() {
        Some(inode) => inode,
        None => return FaultOutcome::Error(MemoryError::NotMapped),
    };
    let file_offset = match area.vm_file_offset(ctx.vpn) {
        Ok(offset) => offset,
        Err(error) => return FaultOutcome::Error(error),
    };
    let file_size = match check_within_file(inode.as_ref(), file_offset) {
        Ok(size) => size,
        Err(error) => return FaultOutcome::Error(error),
    };

    let pc = match inode.ensure_page_cache() {
        Some(pc) => pc,
        None => return FaultOutcome::Error(MemoryError::BackingStoreFailure),
    };
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let fault_around_pages = bounded_fault_around_pages(area, ctx, file_offset, file_size);
    let cache_frame =
        match pc.try_frame_for_filemap_read_ahead(page_index, file_size, fault_around_pages) {
            Ok(frame) => {
                crate::task::perf::record_filemap_ready_hit();
                frame
            }
            Err(PageCacheFault::Retry(wait)) => {
                crate::task::perf::record_filemap_not_ready_retry();
                return FaultOutcome::Retry(wait);
            }
            Err(PageCacheFault::Error(error)) => return FaultOutcome::Error(map_pc_error(error)),
        };
    let cache_ppn = cache_frame.ppn;
    crate::task::perf::FILEMAP_FAULT_FRAMES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    // 可写文件映射首次只给只读 PTE：私有映射写入时触发 CoW，共享映射写入时
    // 触发 page cache dirty 标记后再恢复 W。
    let map_perm = if area.vm_perm().contains(MapPermission::W) {
        area.vm_perm().difference(MapPermission::W)
    } else {
        area.vm_perm()
    };

    if area.inner.alloc_in_memory(ctx.vpn, cache_frame).is_err() {
        return FaultOutcome::Error(MemoryError::AlreadyAllocated);
    }

    let _map_start = crate::task::perf::perf_time_now();
    if let Err(err) = mapper.map_user_page(ctx.vpn, cache_ppn, map_perm) {
        area.inner.remove_in_memory(&ctx.vpn);
        return FaultOutcome::Error(err);
    }
    pc.map_page(page_index);
    map_resident_filemap_tail(
        area,
        mapper,
        &pc,
        ctx.vpn,
        page_index,
        fault_around_pages,
        file_size,
        map_perm,
    );
    let map_ticks = crate::task::perf::perf_time_now().wrapping_sub(_map_start);
    crate::task::perf::FILEMAP_MAP_USER_TICKS
        .fetch_add(map_ticks, core::sync::atomic::Ordering::Relaxed);

    crate::task::perf::FILEMAP_FAULT_TICKS.fetch_add(
        crate::task::perf::perf_time_now().wrapping_sub(_pf_start),
        core::sync::atomic::Ordering::Relaxed,
    );
    match verify_filemap_fault(area, mapper, ctx, cache_ppn) {
        Ok(pa) => FaultOutcome::Completed(pa),
        Err(error) => FaultOutcome::Error(error),
    }
}

/// 处理 `MAP_SHARED` 文件页的写 fault。
///
/// # Semantics
///
/// 通过 page cache 获取可写帧，确保后端缓存被标记为写路径，再把共享帧映射回用户页表。
pub(super) fn filemap_shared_write_fault<T: PageTable>(
    area: &mut Vma,
    mapper: &mut UserMapper<'_, T>,
    ctx: FaultContext,
) -> FaultOutcome {
    crate::task::perf::record_filemap_shared_write_fault();
    let _pf_start = crate::task::perf::perf_time_now();
    let inode = match area.vm_file() {
        Some(inode) => inode,
        None => return FaultOutcome::Error(MemoryError::NotMapped),
    };
    let file_offset = match area.vm_file_offset(ctx.vpn) {
        Ok(offset) => offset,
        Err(error) => return FaultOutcome::Error(error),
    };
    if let Err(error) = check_within_file(inode.as_ref(), file_offset) {
        return FaultOutcome::Error(error);
    }

    let pc = match inode.ensure_page_cache() {
        Some(pc) => pc,
        None => return FaultOutcome::Error(MemoryError::BackingStoreFailure),
    };
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let cache_frame = match pc.try_frame_for_write(page_index) {
        Ok(frame) => frame,
        Err(PageCacheFault::Retry(wait)) => {
            crate::task::perf::record_filemap_not_ready_retry();
            return FaultOutcome::Retry(wait);
        }
        Err(PageCacheFault::Error(error)) => return FaultOutcome::Error(map_pc_error(error)),
    };
    let cache_ppn = cache_frame.ppn;
    crate::task::perf::FILEMAP_FAULT_FRAMES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    if let Err(error) = area.inner.alloc_in_memory(ctx.vpn, cache_frame) {
        return FaultOutcome::Error(error);
    }

    let _map_start = crate::task::perf::perf_time_now();
    if let Err(err) = mapper.map_user_page(ctx.vpn, cache_ppn, area.vm_perm()) {
        area.inner.remove_in_memory(&ctx.vpn);
        return FaultOutcome::Error(err);
    }
    pc.map_page(page_index);
    let map_ticks = crate::task::perf::perf_time_now().wrapping_sub(_map_start);
    crate::task::perf::FILEMAP_MAP_USER_TICKS
        .fetch_add(map_ticks, core::sync::atomic::Ordering::Relaxed);

    crate::task::perf::FILEMAP_FAULT_TICKS.fetch_add(
        crate::task::perf::perf_time_now().wrapping_sub(_pf_start),
        core::sync::atomic::Ordering::Relaxed,
    );
    match verify_filemap_fault(area, mapper, ctx, cache_ppn) {
        Ok(pa) => FaultOutcome::Completed(pa),
        Err(error) => FaultOutcome::Error(error),
    }
}
