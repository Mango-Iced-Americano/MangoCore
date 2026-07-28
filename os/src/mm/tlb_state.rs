//! 用户地址空间的跨 CPU TLB 可见性状态。
//!
//! 本模块只建立激活侧的生产协议：CPU 在返回用户态前先登记自己可能缓存该
//! MM 的翻译，再读取 generation；若本 CPU 尚未观察到当前 generation，就先
//! 做一次本地全用户 TLB 失效。PTE 修改侧将在下一工作包把 generation 推进、
//! 目标快照和延迟 frame 交给锁外 shootdown 提交对象。

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_MM_ID: AtomicUsize = AtomicUsize::new(1);

/// 一个进程地址空间共享的 TLB 代际状态。
///
/// `cached_cpus` 有意采用单调增加的保守集合，而不是瞬时 running mask。MAX_CPUS
/// 只有 8，额外 IPI 成本可控；在尚未实现严密 switch-out 握手前，绝不清 bit
/// 可以避免把仍留有旧翻译的 CPU 错误排除在后续 shootdown 之外。
pub(crate) struct MmTlbState {
    mm_id: usize,
    cached_cpus: AtomicUsize,
    generation: AtomicUsize,
    observed: [AtomicUsize; crate::smp::MAX_CPUS],
}

impl MmTlbState {
    pub(crate) fn new_shared() -> Arc<Self> {
        let mm_id = NEXT_MM_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(mm_id, 0, "MM identifier space exhausted");
        Arc::new(Self {
            mm_id,
            cached_cpus: AtomicUsize::new(0),
            // 代际从 1 开始，使每个新 MM 第一次返回用户态都经过一次明确失效。
            generation: AtomicUsize::new(1),
            observed: [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS],
        })
    }

    /// 登记当前 CPU，并保证它在使用页表根之前观察到最新代际。
    ///
    /// 顺序不能改成“先读 generation、后加 mask”：PTE 修改方可能恰好在两步
    /// 之间推进代际并快照目标，从而既不向本 CPU 发 IPI，本 CPU 也不主动刷新。
    pub(crate) fn attach_cpu(&self, cpu_id: usize) -> usize {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        debug_assert_ne!(self.mm_id, 0);
        let cpu_bit = 1usize << cpu_id;
        let cached = self.cached_cpus.fetch_or(cpu_bit, Ordering::AcqRel) | cpu_bit;

        loop {
            let generation = self.generation.load(Ordering::Acquire);
            if self.observed[cpu_id].load(Ordering::Acquire) != generation {
                // 当前 LA64 ASID 仍归 TCB，因此这里只能清本 CPU 全部 non-global
                // 项；RV64 同样先采用全量失效，精确 range/ASID 留待后续优化。
                crate::hal::user_tlb_invalidate();
                self.observed[cpu_id].store(generation, Ordering::Release);
            }
            // 为下一工作包保留完整闭环：若代际在 flush 后再次推进，就继续刷新，
            // 不能把一次旧 flush 记成对新代际的观察。
            if self.generation.load(Ordering::Acquire) == generation {
                return cached;
            }
        }
    }
}
