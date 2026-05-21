use super::page_fault::FaultContext;
use super::user_mapper::UserMapper;
use super::vma::Vma;
use super::{MapPermission, MemoryError, PageTable, PhysAddr, PhysPageNum};
use crate::config::{PAGE_SIZE, PAGE_SIZE_BITS};
use crate::fs::vfs::IndexNode;
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

fn zero_tail(file_size: usize, file_offset: usize, buf: &mut [u8]) {
    let page_end = file_offset + PAGE_SIZE;
    if page_end > file_size {
        let valid = file_size.saturating_sub(file_offset);
        let tail_start = valid.min(PAGE_SIZE);
        buf[tail_start..].fill(0);
    }
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
    page_table: &mut T,
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

    let mapped_ppn = UserMapper::new(page_table)
        .translate(ctx.vpn)
        .ok_or(MemoryError::NotMapped)?;
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

pub(super) fn filemap_private_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .page_cache()
        .ok_or(MemoryError::BackingStoreFailure)?;
    let cache_frame = pc
        .frame_for_read(file_offset >> PAGE_SIZE_BITS)
        .map_err(map_pc_error)?;

    let allocated_ppn = area.map_one_zeroed_unchecked(page_table, ctx.vpn)?;
    let src = cache_frame.ppn.get_bytes_array();
    let dst = allocated_ppn.get_bytes_array();
    dst.copy_from_slice(src);
    zero_tail(file_size, file_offset, dst);

    verify_filemap_fault(area, page_table, ctx, allocated_ppn)
}

pub(super) fn filemap_read_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .page_cache()
        .ok_or(MemoryError::BackingStoreFailure)?;
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let cache_frame = pc.frame_for_read(page_index).map_err(map_pc_error)?;
    let cache_ppn = cache_frame.ppn;

    // Zero the tail beyond EOF for the last partial page (shared via page cache).
    zero_tail(file_size, file_offset, cache_ppn.get_bytes_array());

    // For both MAP_PRIVATE and MAP_SHARED with W: clear W so first store
    // goes through a fault, triggering CoW (private) or dirty-mark (shared).
    let map_perm = if area.vm_perm().contains(MapPermission::W) {
        area.vm_perm().difference(MapPermission::W)
    } else {
        area.vm_perm()
    };

    area.inner
        .alloc_in_memory(ctx.vpn, cache_frame)
        .map_err(|_| MemoryError::AlreadyAllocated)?;

    if let Err(err) = UserMapper::new(page_table).map_user_page(ctx.vpn, cache_ppn, map_perm) {
        area.inner.remove_in_memory(&ctx.vpn);
        return Err(err);
    }

    verify_filemap_fault(area, page_table, ctx, cache_ppn)
}

pub(super) fn filemap_shared_write_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let _file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .page_cache()
        .ok_or(MemoryError::BackingStoreFailure)?;
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let cache_frame = pc.frame_for_write(page_index).map_err(map_pc_error)?;
    let cache_ppn = cache_frame.ppn;

    area.inner
        .alloc_in_memory(ctx.vpn, cache_frame)
        .map_err(|_| MemoryError::AlreadyAllocated)?;

    if let Err(err) =
        UserMapper::new(page_table).map_user_page(ctx.vpn, cache_ppn, area.vm_perm())
    {
        area.inner.remove_in_memory(&ctx.vpn);
        return Err(err);
    }

    verify_filemap_fault(area, page_table, ctx, cache_ppn)
}
