//! 内核地址空间专用页表映射封装。
//!
//! 相比通用 `PageMapper`，本模块只暴露内核初始化和内核动态映射需要的操作，
//! 包括恒等映射、内核栈/程序页映射以及 dirty 位维护。
//!
//! # TLB
//!
//! PTE 修改仍通过 `PageMapper` 进入具体 `PageTable` 实现，TLB 刷新语义由
//! 架构页表层统一保证。

use super::{
    mapper::PageMapper, MapPermission, MmResult, PageTable, PhysPageNum, VPNRange, VirtAddr,
    VirtPageNum,
};

/// 对内核页表执行受限映射操作的适配器。
pub(super) struct KernelMapper<'a, T: PageTable> {
    mapper: PageMapper<'a, T>,
}

impl<'a, T: PageTable> KernelMapper<'a, T> {
    /// 绑定一个内核页表。
    pub(super) fn new(page_table: &'a mut T) -> Self {
        Self {
            mapper: PageMapper::new(page_table),
        }
    }

    /// 映射单个内核虚拟页到指定物理页。
    pub(super) fn map_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.mapper.map(vpn, ppn, flags)
    }

    /// 建立单页恒等映射。
    pub(super) fn map_identical_page(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.mapper.map_identical(vpn, PhysPageNum(vpn.0), flags);
        Ok(())
    }

    /// 对 `[start_va, end_va)` 范围建立恒等映射。
    ///
    /// # Errors
    ///
    /// 任意单页映射失败时立即返回错误，调用者负责在需要时回滚已经完成的映射。
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

    /// Map a virtual range to a physically contiguous range of equal length.
    pub(super) fn map_range(
        &mut self,
        start_va: VirtAddr,
        start_pa: PhysPageNum,
        end_va: VirtAddr,
        flags: MapPermission,
    ) -> MmResult<()> {
        let start_vpn = start_va.floor();
        let end_vpn = end_va.ceil();
        for (offset, vpn) in VPNRange::new(start_vpn, end_vpn).into_iter().enumerate() {
            self.map_page(vpn, PhysPageNum(start_pa.0 + offset), flags)?;
        }
        Ok(())
    }

    /// 如果单页已映射则解除映射。
    pub(super) fn unmap_page_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        self.mapper.unmap_if_mapped(vpn)
    }

    /// 清除内核映射的 dirty 位。
    pub(super) fn clear_dirty_bit(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.mapper.clear_dirty_bit(vpn)
    }
}
