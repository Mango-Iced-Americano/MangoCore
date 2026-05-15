use super::{
    mapper::PageMapper, MapPermission, MmResult, PageTable, PhysPageNum, VirtAddr, VirtPageNum,
    VPNRange,
};

pub(super) struct KernelMapper<'a, T: PageTable> {
    mapper: PageMapper<'a, T>,
}

impl<'a, T: PageTable> KernelMapper<'a, T> {
    pub(super) fn new(page_table: &'a mut T) -> Self {
        Self {
            mapper: PageMapper::new(page_table),
        }
    }

    pub(super) fn map_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.mapper.map(vpn, ppn, flags)
    }

    pub(super) fn map_identical_page(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.mapper.map_identical(vpn, PhysPageNum(vpn.0), flags);
        Ok(())
    }

    pub(super) fn map_identical_range(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        flags: MapPermission,
    ) -> MmResult<()> {
        for vpn in VPNRange::new(start_va.floor(), end_va.ceil()) {
            self.map_identical_page(vpn, flags)?;
        }
        Ok(())
    }

    pub(super) fn unmap_page_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        self.mapper.unmap_if_mapped(vpn)
    }

    pub(super) fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.mapper.clear_dirty_bit(vpn)
    }
}
