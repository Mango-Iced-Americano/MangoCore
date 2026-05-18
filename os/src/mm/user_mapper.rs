use super::{
    mapper::PageMapper, MapPermission, MemoryError, MmResult, PageTable, PhysPageNum, VirtPageNum,
};

pub(super) struct UserMapper<'a, T: PageTable> {
    mapper: PageMapper<'a, T>,
}

impl<'a, T: PageTable> UserMapper<'a, T> {
    pub(super) fn new(page_table: &'a mut T) -> Self {
        Self {
            mapper: PageMapper::new(page_table),
        }
    }

    fn check_user_flags(flags: MapPermission) -> MmResult<()> {
        if flags.contains(MapPermission::U) {
            Ok(())
        } else {
            Err(MemoryError::NoPermission)
        }
    }

    pub(super) fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.mapper.is_mapped(vpn)
    }

    pub(super) fn map_user_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        Self::check_user_flags(flags)?;
        self.mapper.map(vpn, ppn, flags)
    }

    pub(super) fn map_privileged_user_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.mapper.map(vpn, ppn, flags)
    }

    pub(super) fn unmap_user_page(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.mapper.unmap(vpn)
    }

    pub(super) fn unmap_user_page_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        self.mapper.unmap_if_mapped(vpn)
    }

    pub(super) fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.mapper.translate(vpn)
    }

    pub(super) fn set_user_flags(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        Self::check_user_flags(flags)?;
        self.mapper.set_flags(vpn, flags)
    }

    pub(super) fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> MmResult<()> {
        self.mapper.set_ppn(vpn, ppn)
    }
}
