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
use super::vma::Vma;
use super::{MapPermission, MemoryError, PageTable, PhysAddr, PhysPageNum};
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::fs::vfs::IndexNode;
use crate::fs::PageCacheFault;
use crate::mm::FaultOutcome;
use crate::utils::error::SyscallErr;

fn round_up_page(size: usize) -> usize {
    size.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
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
    let allocated_ppn = match area.alloc_one_zeroed_unmapped(ctx.vpn) {
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
    let cache_frame = match pc.try_frame_for_filemap_read(page_index, file_size) {
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
    let inode = match area.vm_file() { Some(inode) => inode, None => return FaultOutcome::Error(MemoryError::NotMapped) };
    let file_offset = match area.vm_file_offset(ctx.vpn) { Ok(offset) => offset, Err(error) => return FaultOutcome::Error(error) };
    if let Err(error) = check_within_file(inode.as_ref(), file_offset) { return FaultOutcome::Error(error); }

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

    if let Err(error) = area.inner.alloc_in_memory(ctx.vpn, cache_frame) { return FaultOutcome::Error(error); }

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
