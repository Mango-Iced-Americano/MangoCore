//! 页表修改的最小封装层。
//!
//! `PageMapper` 把内核映射操作转发到具体架构的 `PageTable` 实现，
//! 并统一把页表错误映射成 `MemoryError`。用户 PTE 写入不经过本层，而是
//! 由 `UserMapper` 通过 `TlbBatch` 提交。
//!
//! # TLB
//!
//! 任何会修改内核 PTE 的方法最终都依赖 `PageTable` 实现完成必要的
//! 本地 TLB 刷新。B16 尚未为本层接入 kernel-global 远端 shootdown。

use super::{MapPermission, MemoryError, MmResult, PageTable, PhysPageNum, VirtPageNum};

/// 对一个可变页表引用执行单页映射操作。
pub struct PageMapper<'a, T: PageTable> {
    page_table: &'a mut T,
}

/// 查询单页翻译结果，不改变页表状态。
pub(super) fn translate_page<T: PageTable>(
    page_table: &T,
    vpn: VirtPageNum,
) -> Option<PhysPageNum> {
    page_table.translate(vpn)
}

impl<'a, T: PageTable> PageMapper<'a, T> {
    /// 绑定一个页表，后续操作都作用于该页表。
    pub fn new(page_table: &'a mut T) -> Self {
        Self { page_table }
    }

    /// 返回 `vpn` 是否已有有效映射。
    pub fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.page_table.is_mapped(vpn)
    }

    /// 建立 `vpn -> ppn` 映射。
    ///
    /// # Errors
    ///
    /// 当页表层发现重复映射、页表页分配失败或架构实现拒绝该映射时返回
    /// `MemoryError`。
    pub fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.page_table.try_map(vpn, ppn, flags)
    }

    /// 建立恒等映射，虚拟页号和物理页号相同。
    pub fn map_identical(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.page_table.map_identical(vpn, ppn, flags)
    }

    /// 解除一个必须存在的映射。
    ///
    /// # Errors
    ///
    /// `vpn` 当前没有映射时返回 `MemoryError::NotMapped`。
    pub fn unmap(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        if !self.page_table.is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        self.page_table.unmap(vpn);
        Ok(())
    }

    /// 如果 `vpn` 已映射则解除映射，并返回是否真的修改了页表。
    pub fn unmap_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        if !self.page_table.is_mapped(vpn) {
            return Ok(false);
        }
        self.page_table.unmap(vpn);
        Ok(true)
    }

    /// 查询 `vpn` 对应的物理页号。
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    /// 只替换已有 PTE 的物理页号。
    ///
    /// # Errors
    ///
    /// `vpn` 没有映射时返回 `MemoryError::NotMapped`。
    pub fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> MmResult<()> {
        self.page_table
            .set_ppn(vpn, ppn)
            .map_err(|_| MemoryError::NotMapped)
    }

    /// 只替换已有 PTE 的权限位。
    ///
    /// # TLB
    ///
    /// 权限变更必须由底层 `PageTable::set_pte_flags` 完成 TLB 刷新。
    pub fn set_flags(&mut self, vpn: VirtPageNum, flags: MapPermission) -> MmResult<()> {
        self.page_table
            .set_pte_flags(vpn, flags)
            .map_err(|_| MemoryError::NotMapped)
    }

    /// 清除已有 PTE 的 dirty 位。
    ///
    /// # Errors
    ///
    /// `vpn` 没有映射时返回 `MemoryError::NotMapped`。
    pub fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .clear_dirty_bit(vpn)
            .map_err(|_| MemoryError::NotMapped)
    }
}
