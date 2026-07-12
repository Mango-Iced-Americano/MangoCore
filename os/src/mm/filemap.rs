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

/// 处理 `MAP_PRIVATE` 文件页的写 fault。
///
/// # Semantics
///
/// 分配一页新的私有物理页，从 page cache 拷贝文件内容，并清零 EOF 之后的页尾。
/// 该页后续按匿名私有页处理，不再共享 page cache 帧。
pub(super) fn filemap_private_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .ensure_page_cache()
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

/// 处理文件映射页的读/执行 fault。
///
/// # Semantics
///
/// 直接映射 page cache 中的物理页。若 VMA 可写，首次映射会清除 W 位，使后续写入
/// 重新进入 fault 路径并区分 CoW 或共享脏页语义。
pub(super) fn filemap_read_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .ensure_page_cache()
        .ok_or(MemoryError::BackingStoreFailure)?;
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let cache_frame = pc.frame_for_read(page_index).map_err(map_pc_error)?;
    let cache_ppn = cache_frame.ppn;

    // EOF 之后的页尾对用户必须读出 0；这里修改的是共享 page cache 帧。
    zero_tail(file_size, file_offset, cache_ppn.get_bytes_array());

    // 可写文件映射首次只给只读 PTE：私有映射写入时触发 CoW，共享映射写入时
    // 触发 page cache dirty 标记后再恢复 W。
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

/// 处理 `MAP_SHARED` 文件页的写 fault。
///
/// # Semantics
///
/// 通过 page cache 获取可写帧，确保后端缓存被标记为写路径，再把共享帧映射回用户页表。
pub(super) fn filemap_shared_write_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let inode = area.vm_file().ok_or(MemoryError::NotMapped)?;
    let file_offset = area.vm_file_offset(ctx.vpn)?;
    let _file_size = check_within_file(inode.as_ref(), file_offset)?;

    let pc = inode
        .ensure_page_cache()
        .ok_or(MemoryError::BackingStoreFailure)?;
    let page_index = file_offset >> PAGE_SIZE_BITS;
    let cache_frame = pc.frame_for_write(page_index).map_err(map_pc_error)?;
    let cache_ppn = cache_frame.ppn;

    area.inner
        .alloc_in_memory(ctx.vpn, cache_frame)
        .map_err(|_| MemoryError::AlreadyAllocated)?;

    if let Err(err) = UserMapper::new(page_table).map_user_page(ctx.vpn, cache_ppn, area.vm_perm())
    {
        area.inner.remove_in_memory(&ctx.vpn);
        return Err(err);
    }

    verify_filemap_fault(area, page_table, ctx, cache_ppn)
}
