//! 用户页表修改的锁内收集器。
//!
//! [`MmuGather`] 只记录“哪些翻译失效”以及“哪些 frame 暂时不能释放”。
//! 它不发送 IPI，也不等待远端 CPU；这些可能阻塞的动作由锁外的
//! [`TlbFlush`](super::TlbFlush) 执行。

use super::{FrameTracker, PageTable, TlbContext, TlbFlush, VirtPageNum};
use alloc::{sync::Arc, vec::Vec};

/// 本轮 PTE 修改需要失效的最小范围。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FlushRange {
    /// 本轮没有修改任何 PTE。
    None,
    /// 只修改了同一个虚拟页；RV64 可以使用页级失效。
    Page(VirtPageNum),
    /// 修改了多个虚拟页，或撤销了页表层级本身。
    Full,
}

/// 一个 `AddressSpace::write()` 锁持有期内的 MMU 修改记录。
///
/// `retired_frames` 持有已经从 PTE/VMA 中移除、但旧 TLB 翻译仍可能访问的
/// frame。正常路径只有 [`TlbFlush::execute`](super::TlbFlush::execute) 在全部
/// 目标 CPU 完成失效后才会清空它。
pub(crate) struct MmuGather {
    range: FlushRange,
    active_cpus_at_begin: usize,
    retired_frames: Vec<Arc<FrameTracker>>,
}

impl MmuGather {
    pub(crate) const fn new() -> Self {
        Self {
            range: FlushRange::None,
            active_cpus_at_begin: 0,
            retired_frames: Vec::new(),
        }
    }

    /// 开始一次持锁写操作，并记住本轮仍可直接返回该 MM 的 CPU。
    ///
    /// CPU 激活也受同一把地址空间锁保护，所以这个 mask 在 `seal()` 前不会变化。
    pub(crate) fn begin(&mut self, active_cpu_mask: usize) {
        assert_eq!(
            self.range,
            FlushRange::None,
            "previous MMU gather was not sealed"
        );
        assert!(
            self.retired_frames.is_empty(),
            "previous MMU gather still owns retired frames"
        );
        self.active_cpus_at_begin = active_cpu_mask;
    }

    /// 记录一个被新建、删除或修改权限/PPN 的用户 PTE。
    pub(crate) fn record_change(&mut self, vpn: VirtPageNum) {
        self.range = match self.range {
            FlushRange::None => FlushRange::Page(vpn),
            FlushRange::Page(first) if first == vpn => FlushRange::Page(first),
            FlushRange::Page(_) | FlushRange::Full => FlushRange::Full,
        };
    }

    /// 页表层级发生变化，后续必须执行全用户失效。
    pub(crate) fn record_full_flush(&mut self) {
        self.range = FlushRange::Full;
    }

    /// 延迟释放一个旧 frame，直到本轮 TLB 失效完成。
    pub(crate) fn retire_frame<T: PageTable>(&mut self, page_table: &T, frame: Arc<FrameTracker>) {
        if self.range == FlushRange::None {
            self.leak_retired_frames();
            core::mem::forget(frame);
            panic!("retired frame without a recorded PTE change");
        }
        if self.retired_frames.try_reserve(1).is_ok() {
            self.retired_frames.push(frame);
            return;
        }

        self.handle_retire_oom(page_table);
        if self.active_cpus_at_begin == 0
            || self.active_cpus_at_begin == 1usize << crate::smp::cpu_id()
        {
            // 没有活跃远端 CPU 时，handle_retire_oom 已证明旧翻译不可再访问该页。
            drop(frame);
        } else {
            core::mem::forget(frame);
            panic!("cannot reserve user TLB retirement entry");
        }
    }

    /// 延迟释放页表根/中间页；它们可能仍被远端硬件 page walk 使用。
    pub(crate) fn retire_frames<T: PageTable>(
        &mut self,
        page_table: &T,
        frames: Vec<Arc<FrameTracker>>,
    ) {
        if self.range == FlushRange::None {
            self.leak_retired_frames();
            core::mem::forget(frames);
            panic!("retired page-table frames without a full TLB flush");
        }
        if self.retired_frames.try_reserve(frames.len()).is_ok() {
            self.retired_frames.extend(frames);
            return;
        }

        self.handle_retire_oom(page_table);
        if self.active_cpus_at_begin == 0
            || self.active_cpus_at_begin == 1usize << crate::smp::cpu_id()
        {
            drop(frames);
        } else {
            core::mem::forget(frames);
            panic!("cannot reserve page-table retirement entries");
        }
    }

    /// 把锁内记录冻结成一个锁外执行对象。
    pub(crate) fn seal<'a>(&mut self, context: &'a TlbContext) -> Option<TlbFlush<'a>> {
        if self.range == FlushRange::None {
            assert!(
                self.retired_frames.is_empty(),
                "retired frame without a recorded PTE change"
            );
            self.active_cpus_at_begin = 0;
            return None;
        }

        let targets = context.active_cpu_mask();
        assert_eq!(
            targets, self.active_cpus_at_begin,
            "TLB target mask changed while the address-space lock was held"
        );
        // ASID 必须和 `range` 在同一个 VM 锁持有期冻结；解锁后可能发生
        // 全局 ASID rollover，不能再从 MM 中临时拼装另一份失效上下文。
        let asid = context.flush_asid(targets);
        let gather = core::mem::replace(self, Self::new());
        // 即使当前没有活跃 CPU，也必须推进 generation：已经切离的 CPU
        // 仍可能缓存旧 ASID 翻译，它在下次进入前要据此执行本地补刷。
        let generation = match context.advance_generation() {
            Some(generation) => generation,
            None => {
                core::mem::forget(gather);
                panic!("MM TLB generation exhausted");
            }
        };
        Some(TlbFlush::new(context, generation, targets, asid, gather))
    }

    /// 丢弃尚未装入共享 `AddressSpace` 的构造期记录。
    ///
    /// 地址空间尚未发布，旧 frame 可以直接释放，构造期记录也无需失效。
    pub(crate) fn discard_unpublished(&mut self) {
        assert_eq!(
            self.active_cpus_at_begin, 0,
            "unpublished address space unexpectedly has active CPUs"
        );
        self.range = FlushRange::None;
        self.retired_frames.clear();
    }

    pub(super) fn range(&self) -> FlushRange {
        self.range
    }

    /// TLB 同步成功后的唯一正常 frame 释放点。
    pub(super) fn release_retired_frames(&mut self) {
        self.retired_frames.clear();
    }

    /// 同步失败或执行对象被遗弃时保留 frame，禁止 panic 展开提前复用物理页。
    pub(super) fn leak_retired_frames(&mut self) {
        let frames = core::mem::take(&mut self.retired_frames);
        core::mem::forget(frames);
    }

    /// 退休队列扩容失败时，只允许在没有远端观察者的情况下同步释放。
    ///
    /// `range` 覆盖本轮已经入队的全部 frame：保持 `Page(vpn)` 说明所有修改都发生在
    /// 同一个 VPN；只要出现第二个 VPN，`record_change()` 就会把范围升级为 `Full`。
    /// 因此下面的一次页级或全量本地失效足以覆盖清空的整个退休队列。
    ///
    /// 应急释放后仍要保留 `range` 交给 `seal()`。本地 flush 只保护当前活跃 CPU；
    /// 已经切离的 CPU 仍可能保留旧 ASID 翻译，必须靠本轮 generation 推进约束下次进入。
    fn handle_retire_oom<T: PageTable>(&mut self, page_table: &T) {
        if self.active_cpus_at_begin == 0 {
            self.retired_frames.clear();
            return;
        }

        if self.active_cpus_at_begin == 1usize << crate::smp::cpu_id() {
            match self.range {
                FlushRange::None => unreachable!("retirement validates the flush range first"),
                FlushRange::Page(vpn) => page_table.flush_tlb_page(vpn),
                FlushRange::Full => page_table.flush_tlb(),
            }
            self.retired_frames.clear();
            return;
        }

        self.leak_retired_frames();
    }
}

impl Default for MmuGather {
    fn default() -> Self {
        Self::new()
    }
}
