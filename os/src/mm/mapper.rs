#![allow(dead_code)]

use super::{
    MapPermission, MemoryError, MmResult, PageTable, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum,
};

pub struct PageMapper<'a, T: PageTable> {
    page_table: &'a mut T,
}

pub(super) fn translate_page<T: PageTable>(
    page_table: &T,
    vpn: VirtPageNum,
) -> Option<PhysPageNum> {
    page_table.translate(vpn)
}

impl<'a, T: PageTable> PageMapper<'a, T> {
    pub fn new(page_table: &'a mut T) -> Self {
        Self { page_table }
    }

    pub fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.page_table.is_mapped(vpn)
    }

    pub fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.page_table.try_map(vpn, ppn, flags)
    }

    pub fn map_identical(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.page_table.map_identical(vpn, ppn, flags)
    }

    pub fn unmap(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        if !self.page_table.is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        self.page_table.unmap(vpn);
        Ok(())
    }

    pub fn unmap_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        if !self.page_table.is_mapped(vpn) {
            return Ok(false);
        }
        self.page_table.unmap(vpn);
        Ok(true)
    }

    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    pub fn translate_addr(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate_va(va)
    }

    pub fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> MmResult<()> {
        self.page_table
            .set_ppn(vpn, ppn)
            .map_err(|_| MemoryError::NotMapped)
    }

    pub fn set_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> MmResult<()> {
        self.page_table
            .set_pte_flags(vpn, flags)
            .map_err(|_| MemoryError::NotMapped)
    }

    pub fn revoke_write(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .revoke_write(vpn)
            .map_err(|_| MemoryError::NotMapped)
    }

    pub fn clear_access_bit(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .clear_access_bit(vpn)
            .map_err(|_| MemoryError::NotMapped)
    }

    pub fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .clear_dirty_bit(vpn)
            .map_err(|_| MemoryError::NotMapped)
    }

    pub fn is_dirty(&self, vpn: VirtPageNum) -> Option<bool> {
        self.page_table.is_dirty(vpn)
    }
}
