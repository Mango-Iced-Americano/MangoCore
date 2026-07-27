//! 用户页表修改的本地 TLB 提交边界。
//!
//! B16 阶段用户任务仍固定在 CPU0，因此这里只完成 unpublished/local-only
//! 语义。`Published` 明确保留为远程 shootdown 门禁：在远程 ack 协议接入前，
//! 任何实际 PTE 修改都会 fail-stop，不能把一次本地刷新伪装成 SMP 正确性。

use super::{FrameTracker, MapPermission, MemoryError, PageTable, PhysPageNum, VirtPageNum};
use alloc::{sync::Arc, vec::Vec};

/// 地址空间当前可能被哪些 CPU 的 TLB 观察到。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TlbPublication {
    /// 页表尚未交给任何任务，硬件不可能缓存其中的翻译。
    Unpublished,
    /// 用户任务仍固定在 CPU0，只需要提交当前 CPU 的本地刷新。
    LocalOnly,
    /// 页表可能被多个 CPU 使用，必须等待远程 shootdown ack。
    Published,
}

/// 一次用户 PTE 修改批次。
///
/// batch 借用页表期间，所有写入都走 raw/no-flush 接口。`Drop` 是错误路径的
/// 兜底提交点，因此 `?` 或提前 `return` 也不会漏掉本地刷新。字段析构发生在
/// `Drop::drop` 返回之后，保证延迟帧严格晚于 TLB 提交释放。
pub(crate) struct TlbBatch<'a, T: PageTable> {
    page_table: &'a mut T,
    publication: TlbPublication,
    first_dirty_vpn: Option<VirtPageNum>,
    needs_full_flush: bool,
    deferred_frames: Vec<Arc<FrameTracker>>,
}

impl<'a, T: PageTable> TlbBatch<'a, T> {
    pub(crate) fn new(page_table: &'a mut T, publication: TlbPublication) -> Self {
        if publication == TlbPublication::Published {
            // Phase 4 接入远程 shootdown 前，在任何 PTE 写入发生前就停止。
            // 这样既不会把本地刷新伪装成 SMP 提交，也不会留下半修改页表。
            panic!("remote TLB shootdown is required for a published address space");
        }
        Self {
            page_table,
            publication,
            first_dirty_vpn: None,
            needs_full_flush: false,
            deferred_frames: Vec::new(),
        }
    }

    /// 记录本批修改范围：只触及一个 VPN 时请求页级刷新，出现第二个 VPN 后
    /// 升级为全量刷新。RV64 可据此保留 page fault/CoW 的单页 fast path；
    /// LA64 在 MM 尚未持有 ASID 的 B16 阶段会由 HAL 保守升级为本核全量刷新。
    fn mark_dirty(&mut self, vpn: VirtPageNum) {
        match self.first_dirty_vpn {
            None => self.first_dirty_vpn = Some(vpn),
            Some(first) if first != vpn => self.needs_full_flush = true,
            Some(_) => {}
        }
    }

    pub(crate) fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.page_table.is_mapped(vpn)
    }

    pub(crate) fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    pub(crate) fn map(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError> {
        self.page_table.try_map_no_flush(vpn, ppn, flags)?;
        self.mark_dirty(vpn);
        Ok(())
    }

    pub(crate) fn unmap(&mut self, vpn: VirtPageNum) -> Result<(), MemoryError> {
        if !self.page_table.is_mapped(vpn) {
            return Err(MemoryError::NotMapped);
        }
        self.page_table.unmap_no_flush(vpn);
        self.mark_dirty(vpn);
        Ok(())
    }

    pub(crate) fn unmap_if_mapped(&mut self, vpn: VirtPageNum) -> Result<bool, MemoryError> {
        if !self.page_table.is_mapped(vpn) {
            return Ok(false);
        }
        self.page_table.unmap_no_flush(vpn);
        self.mark_dirty(vpn);
        Ok(true)
    }

    pub(crate) fn block_write(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        let ppn = self.page_table.block_and_ret_mut_no_flush(vpn);
        if ppn.is_some() {
            self.mark_dirty(vpn);
        }
        ppn
    }

    pub(crate) fn set_ppn(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
    ) -> Result<(), MemoryError> {
        self.page_table
            .set_ppn_no_flush(vpn, ppn)
            .map_err(|_| MemoryError::NotMapped)?;
        self.mark_dirty(vpn);
        Ok(())
    }

    pub(crate) fn set_flags(
        &mut self,
        vpn: VirtPageNum,
        flags: MapPermission,
    ) -> Result<(), MemoryError> {
        self.page_table
            .set_pte_flags_no_flush(vpn, flags)
            .map_err(|_| MemoryError::NotMapped)?;
        self.mark_dirty(vpn);
        Ok(())
    }

    pub(crate) fn set_dirty(&mut self, vpn: VirtPageNum) -> Result<(), MemoryError> {
        self.page_table
            .set_dirty_bit_no_flush(vpn)
            .map_err(|_| MemoryError::NotMapped)?;
        self.mark_dirty(vpn);
        Ok(())
    }

    /// 把已从 VMA 所有权中移出的物理页留到 TLB 提交之后再释放。
    pub(crate) fn defer_frame(&mut self, frame: Arc<FrameTracker>) {
        if self.deferred_frames.try_reserve(1).is_err() {
            // 低内存时允许提前结束当前小批次，但仍严格保持“先刷新、后释放”。
            // 这只牺牲合并刷新效率，不改变页表或 frame 生命周期语义。
            self.flush_pending();
            drop(frame);
            return;
        }
        self.deferred_frames.push(frame);
    }

    fn flush_pending(&mut self) {
        let Some(first_dirty_vpn) = self.first_dirty_vpn else {
            assert!(
                self.deferred_frames.is_empty(),
                "deferred frame without a PTE update"
            );
            return;
        };
        match self.publication {
            TlbPublication::Unpublished => {}
            TlbPublication::LocalOnly if self.needs_full_flush => self.page_table.flush_tlb(),
            TlbPublication::LocalOnly => self.page_table.flush_tlb_page(first_dirty_vpn),
            TlbPublication::Published => unreachable!("Published batches fail in TlbBatch::new"),
        }
        self.first_dirty_vpn = None;
        self.needs_full_flush = false;
        self.deferred_frames.clear();
    }

    /// 显式结束批次；即使调用方省略，`Drop` 仍会执行同一提交逻辑。
    pub(crate) fn commit(mut self) {
        self.flush_pending();
    }
}

impl<T: PageTable> Drop for TlbBatch<'_, T> {
    fn drop(&mut self) {
        self.flush_pending();
    }
}
