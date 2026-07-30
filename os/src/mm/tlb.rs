//! 用户地址空间的跨 CPU TLB 同步状态。
//!
//! [`TlbContext`] 由共享地址空间持有，记录缓存过该 MM 的 CPU 与代际；
//! [`TlbFlush`] 则是从 VM 锁中带出的单次执行对象。两者的边界保证 IPI/ack
//! 等待永远不发生在地址空间锁内。

use super::{FlushRange, MmuGather};
#[cfg(target_arch = "loongarch64")]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_MM_ID: AtomicUsize = AtomicUsize::new(1);

/// 一个共享地址空间的长期 TLB 代际状态。
pub(crate) struct TlbContext {
    mm_id: usize,
    cached_cpus: AtomicUsize,
    generation: AtomicUsize,
    observed: [AtomicUsize; crate::smp::MAX_CPUS],
    /// LoongArch 软件 epoch 与硬件 ASID 的原子组合；0 表示尚未分配。
    #[cfg(target_arch = "loongarch64")]
    asid_context: AtomicU64,
}

/// 一次已经脱离 VM 锁的 TLB 失效工作。
///
/// `gather` 继续拥有所有退休 frame；`execute()` 只有在本地/远端失效和 ack
/// 全部完成后才释放它们。对象被丢弃或同步失败时会故意泄漏 frame 并 fail-stop。
pub(crate) struct TlbFlush<'a> {
    context: &'a TlbContext,
    generation: Option<usize>,
    targets: usize,
    /// 与本轮 PTE 修改在同一 VM 锁内冻结的硬件 ASID。
    asid: u16,
    gather: MmuGather,
    executed: bool,
}

impl TlbContext {
    pub(crate) fn new() -> Self {
        let mm_id = NEXT_MM_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(mm_id, 0, "MM identifier space exhausted");
        Self {
            mm_id,
            cached_cpus: AtomicUsize::new(0),
            // 从 1 开始，使新 MM 第一次返回用户态时必定执行一次明确失效。
            generation: AtomicUsize::new(1),
            observed: [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS],
            #[cfg(target_arch = "loongarch64")]
            asid_context: AtomicU64::new(0),
        }
    }

    /// 返回当前 MM 的硬件 ASID；`None` 表示必须先在无普通锁状态完成 rollover。
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn assign_asid(&self) -> Option<u16> {
        let current = self.asid_context.load(Ordering::Acquire);
        let assigned = crate::hal::arch::loongarch64::tlb::try_assign_asid(current)?;
        if assigned != current {
            self.asid_context.store(assigned, Ordering::Release);
        }
        Some(crate::hal::arch::loongarch64::tlb::hardware_asid(assigned))
    }

    /// RV64 尚未使用 SATP.ASID；返回 0 让上层维持统一的用户 VM 上下文接口。
    #[cfg(not(target_arch = "loongarch64"))]
    pub(crate) fn assign_asid(&self) -> Option<u16> {
        Some(0)
    }

    /// 返回曾经缓存过该 MM 的 CPU；未实现安全 detach 前该集合只增不减。
    pub(crate) fn cached_cpu_mask(&self) -> usize {
        self.cached_cpus.load(Ordering::Acquire)
    }

    /// 取得本轮页级失效必须使用的硬件 ASID。
    ///
    /// 有缓存者意味着该 MM 至少完成过一次 `activate_on()`，因此 LA64 必须
    /// 已拥有非零 ASID。调用者在 VM 锁内取值，使 ASID 与本轮 VPN 成为同一快照。
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn flush_asid(&self, targets: usize) -> u16 {
        if targets == 0 {
            return 0;
        }
        let asid = crate::hal::arch::loongarch64::tlb::hardware_asid(
            self.asid_context.load(Ordering::Acquire),
        );
        assert_ne!(asid, 0, "cached LoongArch MM has no hardware ASID");
        asid
    }

    /// RV64 在 B27 前仍使用 ASID 0；页级 `sfence.vma` 会匹配全部 ASID。
    #[cfg(not(target_arch = "loongarch64"))]
    pub(crate) const fn flush_asid(&self, _targets: usize) -> u16 {
        0
    }

    /// 登记当前 CPU，并保证它在使用页表根之前观察到最新代际。
    pub(crate) fn activate_cpu(&self, cpu_id: usize) {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        let cpu_bit = 1usize << cpu_id;
        let cached = self.cached_cpus.load(Ordering::Acquire);
        if cached & cpu_bit == 0 {
            // 激活与修改都持有同一把地址空间锁；首次登记不会漏过修改方快照。
            self.cached_cpus.fetch_or(cpu_bit, Ordering::AcqRel);
        }

        loop {
            let generation = self.generation.load(Ordering::Acquire);
            let observed = self.observed[cpu_id].load(Ordering::Acquire);
            if observed < generation {
                crate::hal::user_tlb_invalidate();
                self.observed[cpu_id].fetch_max(generation, Ordering::Release);
            }
            // 若修改方在本次 flush 后推进了代际，必须重新失效后才能使用页表根。
            if self.generation.load(Ordering::Acquire) == generation {
                return;
            }
        }
    }

    /// 为一次已经记录到 PTE 的修改分配新代际。
    pub(crate) fn advance_generation(&self) -> Option<usize> {
        self.generation
            .fetch_update(Ordering::Release, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()
            .map(|previous| previous + 1)
    }

    /// ack 已证明目标 CPU 完成失效，单调发布它们观察到的代际。
    fn acknowledge(&self, generation: usize, targets: usize) {
        for cpu_id in 0..crate::smp::configured_cpu_count() {
            if targets & (1usize << cpu_id) != 0 {
                // 同一 MM 的两代修改可能并发等待；旧一代晚完成时不能覆盖新值。
                self.observed[cpu_id].fetch_max(generation, Ordering::Release);
            }
        }
    }
}

impl Default for TlbContext {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> TlbFlush<'a> {
    pub(super) fn new(
        context: &'a TlbContext,
        generation: Option<usize>,
        targets: usize,
        asid: u16,
        gather: MmuGather,
    ) -> Self {
        Self {
            context,
            generation,
            targets,
            asid,
            gather,
            executed: false,
        }
    }

    /// 在不持普通锁的上下文完成失效，并在最后释放退休 frame。
    pub(crate) fn execute(mut self) {
        if let Some(generation) = self.generation {
            let current_bit = 1usize << crate::smp::cpu_id();
            let result = if self.targets == current_bit {
                match self.gather.range() {
                    FlushRange::None => panic!("TLB flush has no recorded PTE change"),
                    FlushRange::Page(vpn) => {
                        crate::hal::user_tlb_invalidate_page(self.asid, vpn)
                    }
                    FlushRange::Full => crate::hal::user_tlb_invalidate(),
                }
                Ok(())
            } else {
                let page = match self.gather.range() {
                    FlushRange::None => panic!("TLB flush has no recorded PTE change"),
                    FlushRange::Page(vpn) => Some(vpn),
                    FlushRange::Full => None,
                };
                crate::smp::synchronize_user_tlb(self.targets, self.asid, page)
            };

            if let Err(error) = result {
                self.gather.leak_retired_frames();
                self.executed = true;
                panic!(
                    "user TLB shootdown failed: mm={} generation={} targets={:#x} error={:?}",
                    self.context.mm_id, generation, self.targets, error
                );
            }
            self.context.acknowledge(generation, self.targets);
        }

        self.executed = true;
        // 这是正常执行中唯一的 frame 退休点：失效和 ack 均已完成。
        self.gather.release_retired_frames();
    }
}

impl Drop for TlbFlush<'_> {
    fn drop(&mut self) {
        if self.executed {
            return;
        }
        self.gather.leak_retired_frames();
        panic!(
            "unexecuted user TLB flush: mm={} generation={:?} targets={:#x}",
            self.context.mm_id, self.generation, self.targets
        );
    }
}
