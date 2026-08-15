//! 用户地址空间专用页表映射封装。
//!
//! `UserMapper` 同时借用原始页表和当前 [`MmuGather`](super::MmuGather)：普通
//! 用户页映射必须带 `MapPermission::U`，少数特权用户资源（如 trap context）
//! 通过显式接口映射。每次 raw PTE 写入后都必须立即向 gather 记录失效范围。
//!
//! # Safety
//!
//! 本模块不直接执行 unsafe 操作，但它维护用户/内核权限边界。新增调用点时必须
//! 明确选择 `map_user_page` 还是 `map_privileged_user_page`，避免把内核专用页暴露给
//! 用户态。

use super::{
    FrameTracker, MapPermission, MemoryError, MmResult, MmuGather, PageTable, PhysPageNum,
    VirtPageNum,
};
use alloc::sync::Arc;

/// 用户页表操作适配器。
///
/// 该类型只在地址空间锁内存活，不拥有页表或 frame；`gather` 才是退休 frame
/// 在锁内的唯一所有者。
pub(crate) struct UserMapper<'a, T: PageTable> {
    page_table: &'a mut T,
    gather: &'a mut MmuGather,
}

/// `perf_diag` 专用的同镜像 A/B 开关；关闭后成功 fresh map（invalid→valid）
/// 不再向 `MmuGather` 记录失效，跳过本地/远端 TLB 同步。正常构建保持现状。
///
/// 只允许 RV64 使用：QEMU TCG 的 softmmu TLB 不缓存“PTE 不存在”的负结果，
/// 每次隐式访问都重走 guest 页表，因此 skip flush 后本地与远端都立即看到
/// 新 PTE；真硬件由 Svvptc（或偶发 spurious-fault 重试）保证最终收敛。
/// 破坏性更新（unmap/PPN 替换/权限收紧/页表层级回收）仍走 `record_change`。
#[cfg(all(feature = "perf_diag", target_arch = "riscv64"))]
fn fresh_map_no_flush_enabled() -> bool {
    static ENABLED: spin::Once<bool> = spin::Once::new();
    *ENABLED.call_once(|| {
        crate::bootargs::get_cmdline()
            .split_ascii_whitespace()
            .any(|arg| arg == "mango.rv.fresh_map_flush=off")
    })
}

#[cfg(not(all(feature = "perf_diag", target_arch = "riscv64")))]
fn fresh_map_no_flush_enabled() -> bool {
    false
}

impl<'a, T: PageTable> UserMapper<'a, T> {
    /// 绑定当前地址空间的页表与 MMU 修改记录。
    pub(super) fn new(page_table: &'a mut T, gather: &'a mut MmuGather) -> Self {
        Self { page_table, gather }
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
        self.page_table.is_mapped(vpn)
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
        self.map_page(vpn, ppn, flags)
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
        self.map_page(vpn, ppn, flags)
    }

    /// 解除一个必须存在的用户页映射。
    pub(super) fn unmap_user_page(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        if !self.page_table.is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        self.page_table.unmap_no_flush(vpn);
        self.gather.record_change(vpn);
        Ok(())
    }

    /// 如果用户页已映射则解除映射。
    pub(super) fn unmap_user_page_if_mapped(&mut self, vpn: VirtPageNum) -> MmResult<bool> {
        if !self.page_table.is_mapped(vpn) {
            return Ok(false);
        }
        self.page_table.unmap_no_flush(vpn);
        self.gather.record_change(vpn);
        Ok(true)
    }

    /// 查询用户虚拟页对应的物理页号。
    pub(super) fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    /// 更新用户 PTE 权限，并强制要求结果仍带 U 位。
    pub(super) fn set_user_flags(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        Self::check_user_flags(flags)?;
        self.page_table
            .set_pte_flags_no_flush(vpn, flags)
            .map_err(|_| MemoryError::NotMapped)?;
        self.gather.record_change(vpn);
        Ok(())
    }

    /// 替换已有用户 PTE 的物理页号。
    pub(super) fn set_ppn(&mut self, vpn: VirtPageNum, ppn: PhysPageNum) -> MmResult<()> {
        self.page_table
            .set_ppn_no_flush(vpn, ppn)
            .map_err(|_| MemoryError::NotMapped)?;
        self.gather.record_change(vpn);
        Ok(())
    }

    /// 撤销写权限并返回原物理页号。
    pub(super) fn block_write(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        let ppn = self.page_table.block_and_ret_mut_no_flush(vpn);
        if ppn.is_some() {
            self.gather.record_change(vpn);
        }
        ppn
    }

    /// 设置 LoongArch 软件 dirty 位并记录对应翻译失效。
    pub(super) fn set_dirty(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .set_dirty_bit_no_flush(vpn)
            .map_err(|_| MemoryError::NotMapped)?;
        self.gather.record_change(vpn);
        Ok(())
    }

    /// 清除硬件/软件 dirty 位并记录 TLB 失效；mkclean 必须和写保护在同一
    /// VM 锁持有期提交，不能让旧 dirty 翻译跨越下一轮 writeback。
    pub(super) fn clear_dirty(&mut self, vpn: VirtPageNum) -> MmResult<()> {
        self.page_table
            .clear_dirty_bit(vpn)
            .map_err(|_| MemoryError::NotMapped)?;
        self.gather.record_change(vpn);
        Ok(())
    }

    /// 保留旧 frame 到锁外 TLB 同步结束。
    pub(super) fn retire_frame(&mut self, frame: Arc<FrameTracker>) {
        self.gather.retire_frame(self.page_table, frame);
    }

    fn map_page(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> MmResult<()> {
        let started = crate::task::perf::perf_memory_io_time_now();
        let result = self.page_table.try_map_no_flush(vpn, ppn, flags);
        crate::task::perf::record_pagefault_stage(
            2,
            crate::task::perf::perf_memory_io_time_now().wrapping_sub(started),
        );
        result?;
        if fresh_map_no_flush_enabled() {
            // fresh map 是严格 invalid→valid（已有 PTE 会被 try_map_no_flush
            // 拒绝），按规范不需要 TLB 失效；见 fresh_map_no_flush_enabled 的
            // 能力边界说明。本轮若已记录破坏性修改，range 仍非空，flush 照常。
            crate::task::perf::record_fresh_map_outcome(true);
        } else {
            crate::task::perf::record_fresh_map_outcome(false);
            self.gather.record_change(vpn);
        }
        Ok(())
    }
}
