//! 用户地址空间专用页表映射封装。
//!
//! `UserMapper` 在 `TlbBatch` 之上加入用户态权限检查：普通用户页映射必须带
//! `MapPermission::U`，少数特权用户资源（如 trap context）通过显式的
//! privileged 接口映射。所有写入都由 batch 统一提交，不能逐页绕过刷新协议。
//!
//! # Safety
//!
//! 本模块不直接执行 unsafe 操作，但它维护用户/内核权限边界。新增调用点时必须
//! 明确选择 `map_user_page` 还是 `map_privileged_user_page`，避免把内核专用页暴露给
//! 用户态。

use super::{MapPermission, MemoryError, MmResult, PageTable, PhysPageNum, TlbBatch, VirtPageNum};

/// 用户页表操作适配器。
pub(super) struct UserMapper<'batch, 'page_table, T: PageTable> {
    batch: &'batch mut TlbBatch<'page_table, T>,
}

impl<'batch, 'page_table, T: PageTable> UserMapper<'batch, 'page_table, T> {
    /// 绑定一个用户页表修改批次。
    pub(super) fn new(batch: &'batch mut TlbBatch<'page_table, T>) -> Self {
        Self { batch }
    }

    fn check_user_flags(flags: MapPermission) -> MmResult<()> {
        if flags.contains(MapPermission::U) {
            Ok(())
        } else {
            Err(MemoryError::NoPermission)
        }
    }

    /// 返回用户虚拟页是否已经有 PTE。
    pub(super) fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.batch.is_mapped(vpn)
    }

    /// 映射一个用户可访问页面。
    ///
    /// # Errors
    ///
    /// `flags` 缺少 `MapPermission::U` 时返回 `MemoryError::NoPermission`；
    /// 其他错误来自底层页表映射。
    pub(super) fn map_user_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        Self::check_user_flags(flags)?;
        self.batch.map(vpn, ppn, flags)
    }

    /// 映射不带 U 位的用户地址空间内部页面。
    ///
    /// # Semantics
    ///
    /// 仅用于 trap context 等被放在进程页表中、但用户态不能直接访问的页面。
    pub(super) fn map_privileged_user_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        self.batch.map(vpn, ppn, flags)
    }

    /// 解除一个必须存在的用户页映射。
    pub(super) fn unmap_user_page(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.batch.unmap(vpn)
    }

    /// 如果用户页已映射则解除映射。
    pub(super) fn unmap_user_page_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        self.batch.unmap_if_mapped(vpn)
    }

    /// 查询用户虚拟页对应的物理页号。
    pub(super) fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.batch.translate(vpn)
    }

    /// 更新用户 PTE 权限，并强制要求结果仍带 U 位。
    pub(super) fn set_user_flags(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        Self::check_user_flags(flags)?;
        self.batch.set_flags(vpn, flags)
    }

    /// 替换已有用户 PTE 的物理页号。
    pub(super) fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> MmResult<()> {
        self.batch.set_ppn(vpn, ppn)
    }
}
