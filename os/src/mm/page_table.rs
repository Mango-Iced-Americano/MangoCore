use super::{MapPermission, MemoryError, PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};

// user-copy 方向，读是从用户拿，写是往用户填
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserAccess {
    Read,
    Write,
    ReadWrite,
}

impl UserAccess {
    #[inline(always)]
    pub fn needs_read(self) -> bool {
        matches!(self, UserAccess::Read | UserAccess::ReadWrite)
    }

    #[inline(always)]
    pub fn needs_write(self) -> bool {
        matches!(self, UserAccess::Write | UserAccess::ReadWrite)
    }
}

// 缺页时区分读写取指
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultAccess {
    Load,
    Store,
    Execute,
}

#[allow(unused)]
pub trait PageTable {
    /// 基本映射操作
    /// map、unmap、translate、translate_va
    /// 通过指定flags将vpn映射到ppn
    /// # 注意
    /// Allocation should be done elsewhere.
    /// # 特例
    /// Panics if the `vpn` is mapped.
    fn try_map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError>;
    fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.try_map(vpn, ppn, flags).unwrap();
    }
    #[inline(always)]
    fn map_identical(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map(vpn, ppn, flags)
    }
    #[allow(unused)]
    /// Unmap the `vpn` to `ppn` with the `flags`.
    /// # Exceptions
    /// Panics if the `vpn` is NOT mapped (invalid).
    fn unmap(&mut self, vpn: VirtPageNum);
    #[inline(always)]
    fn unmap_identical(&mut self, vpn: VirtPageNum) {
        self.unmap(vpn)
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` if exists
    /// `None` is returned if nothing is found.
    fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum>;
    /// Translate the virtual address into its corresponding `PhysAddr` if mapped in current page table.
    /// `None` is returned if nothing is found.
    fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr>;
    fn block_and_ret_mut(&self, vpn: VirtPageNum) -> Option<PhysPageNum>;
    /// Return the physical token to current page.
    fn token(&self) -> usize;
    fn revoke_read(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn revoke_write(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn revoke_execute(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> Result<(), ()>;
    fn set_pte_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> Result<(), ()>;
    fn clear_access_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()>;
    fn new() -> Self;
    #[inline(always)]
    fn new_kern_space() -> Self
    where
        Self: Sized,
    {
        Self::new()
    }
    /// Release all page table frames to the frame allocator.
    /// Used when a zombie process no longer needs its address space.
    fn release_frames(&mut self);
    /// Create an empty page table from `satp`
    /// # Argument
    /// * `satp` Supervisor Address Translation & Protection reg. that points to the physical page containing the root page.
    fn from_token(satp: usize) -> Self;
    /// Predicate for the valid bit.
    fn is_mapped(&self, vpn: VirtPageNum) -> bool;
    fn activate(&self);
    fn is_valid(&self, vpn: VirtPageNum) -> Option<bool>;
    fn is_dirty(&self, vpn: VirtPageNum) -> Option<bool>;
    fn readable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn writable(&self, vpn: VirtPageNum) -> Option<bool>;
    fn executable(&self, vpn: VirtPageNum) -> Option<bool>;
    // 只看 PTE 用户位和读写位
    fn user_access_ok(&self, vpn: VirtPageNum, access: UserAccess) -> Option<bool>;
}

#[allow(unused)]
pub fn gen_start_end(start: VirtAddr, end: VirtAddr) -> (VirtPageNum, VirtPageNum) {
    (start.floor(), end.ceil())
}

// 老工具函数先留着
pub fn ptf_ok(ptf: usize) -> bool {
    ptf & 1 == 1
}
