#![allow(dead_code)]

use super::vma::Vma;
use super::page_fault::FaultContext;
use super::{MemoryError, PageMapper, PageTable, PhysAddr, PhysPageNum, VirtAddr};
use crate::config::PAGE_SIZE;
use crate::fs::{file_trait::File, SeekWhence};
use alloc::sync::Arc;

struct FileMapFault {
    file: Arc<dyn File>,
    old_offset: usize,
    file_offset: usize,
}

impl FileMapFault {
    fn new<T: PageTable>(area: &Vma, ctx: FaultContext) -> Result<Self, MemoryError> {
        let file = area.vm_file().ok_or(MemoryError::NotMapped)?;
        let old_offset = file
            .lseek(0, SeekWhence::SEEK_CUR)
            .map_err(|_| MemoryError::BadAddress)?;
        let page_start_va = VirtAddr::from(ctx.vpn).0;
        let area_start_va = VirtAddr::from(area.get_start::<T>()).0;
        let offset_in_area = page_start_va - area_start_va;
        let file_offset = old_offset
            .checked_add(offset_in_area)
            .ok_or(MemoryError::BeyondEOF)?;

        if file_offset > rounded_file_page_end(file.get_size()) {
            return Err(MemoryError::BeyondEOF);
        }

        Ok(Self {
            file,
            old_offset,
            file_offset,
        })
    }

    fn seek_to_fault_page(&self) -> Result<(), MemoryError> {
        if self.file_offset > isize::MAX as usize || self.old_offset > isize::MAX as usize {
            return Err(MemoryError::BadAddress);
        }
        self.file
            .lseek(self.file_offset as isize, SeekWhence::SEEK_SET)
            .map(|_| ())
            .map_err(|_| MemoryError::BadAddress)
    }

    fn restore_old_offset(&self) -> Result<(), MemoryError> {
        self.file
            .lseek(self.old_offset as isize, SeekWhence::SEEK_SET)
            .map(|_| ())
            .map_err(|_| MemoryError::BadAddress)
    }
}

pub(super) fn filemap_private_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let file_fault = FileMapFault::new::<T>(area, ctx)?;
    let allocated_ppn = area.map_one_zeroed_unchecked(page_table, ctx.vpn)?;
    file_fault.seek_to_fault_page()?;
    file_fault.file.read(None, page_bytes_mut(allocated_ppn));
    file_fault.restore_old_offset()?;
    Ok(ctx.offset_phys(allocated_ppn))
}

pub(super) fn filemap_read_fault<T: PageTable>(
    area: &mut Vma,
    page_table: &mut T,
    ctx: FaultContext,
) -> Result<PhysAddr, MemoryError> {
    let file_fault = FileMapFault::new::<T>(area, ctx)?;
    let cache_phys_page = file_fault
        .file
        .get_single_cache(file_fault.file_offset)
        .map_err(|_| MemoryError::BeyondEOF)?
        .lock()
        .get_tracker();
    let cache_ppn = cache_phys_page.ppn;

    area.inner.alloc_in_memory(ctx.vpn, cache_phys_page)?;
    if let Err(err) = PageMapper::new(page_table).map(ctx.vpn, cache_ppn, area.vm_perm()) {
        area.inner.remove_in_memory(&ctx.vpn);
        return Err(err);
    }

    Ok(ctx.offset_phys(cache_ppn))
}

fn rounded_file_page_end(file_size: usize) -> usize {
    file_size.saturating_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

fn page_bytes_mut(ppn: PhysPageNum) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(PhysAddr::from(ppn).0 as *mut u8, PAGE_SIZE) }
}
