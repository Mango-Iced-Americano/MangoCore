//! 用户地址空间的跨 CPU TLB 同步状态。
//!
//! [`TlbContext`] 由共享地址空间持有，记录正在使用该 MM 的 CPU 与代际；
//! [`TlbFlush`] 则是从 VM 锁中带出的单次执行对象。两者的边界保证 IPI/ack
//! 等待永远不发生在地址空间锁内。

use super::{FlushRange, MmuGather};
use core::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};

static NEXT_MM_ID: AtomicUsize = AtomicUsize::new(1);

/// 一个共享地址空间的长期 TLB 代际状态。
pub(crate) struct TlbContext {
    mm_id: usize,
    active_cpus: AtomicUsize,
    generation: AtomicUsize,
    observed: [AtomicUsize; crate::smp::MAX_CPUS],
    /// 架构软件 epoch 与硬件 ASID 的原子组合；0 表示尚未分配或无 ASID。
    asid_context: AtomicU64,
}

/// 一次已经脱离 VM 锁的 TLB 失效工作。
///
/// `gather` 继续拥有所有退休 frame；`execute()` 只有在本地/远端失效和 ack
/// 全部完成后才释放它们。对象被丢弃或同步失败时会故意泄漏 frame 并 fail-stop。
pub(crate) struct TlbFlush<'a> {
    context: &'a TlbContext,
    generation: usize,
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
            active_cpus: AtomicUsize::new(0),
            // 从 1 开始，使新 MM 第一次返回用户态时必定执行一次明确失效。
            generation: AtomicUsize::new(1),
            observed: [const { AtomicUsize::new(0) }; crate::smp::MAX_CPUS],
            asid_context: AtomicU64::new(0),
        }
    }

    /// 返回地址空间生命周期内唯一且非零的诊断编号。
    pub(crate) fn id(&self) -> usize {
        self.mm_id
    }

    /// 返回当前 MM 的硬件 ASID 与软件 epoch；`None` 表示必须先在无普通锁状态
    /// 完成 rollover。LA64 不需要把 epoch 交给切页表路径，固定返回 0。
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn assign_asid(&self) -> Option<(u16, u64)> {
        let current = self.asid_context.load(Ordering::Acquire);
        let assigned = crate::hal::arch::loongarch64::tlb::try_assign_asid(current)?;
        if assigned != current {
            self.asid_context.store(assigned, Ordering::Release);
        }
        Some((
            crate::hal::arch::loongarch64::tlb::hardware_asid(assigned),
            0,
        ))
    }

    /// RISC-V 从 SATP.ASID 分配 MM-owned context；ASIDLEN=0 时安全退化为 0。
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn assign_asid(&self) -> Option<(u16, u64)> {
        let current = self.asid_context.load(Ordering::Acquire);
        let assigned = crate::hal::arch::riscv::sv39::try_assign_asid(current)?;
        if assigned != current {
            self.asid_context.store(assigned, Ordering::Release);
        }
        Some((
            crate::hal::arch::riscv::sv39::hardware_asid(assigned),
            assigned >> 16,
        ))
    }

    /// 返回当前仍可能直接返回该 MM 用户态的 CPU。
    pub(crate) fn active_cpu_mask(&self) -> usize {
        self.active_cpus.load(Ordering::Acquire)
    }

    /// 仅供诊断判断指定 CPU 是否已经观察到当前代际。
    ///
    /// 调用方仍需通过 `AddressSpace` 锁冻结 generation；这个布尔值不能代替
    /// 激活协议，也不能作为提前释放 frame 的依据。
    pub(crate) fn cpu_is_current(&self, cpu_id: usize) -> bool {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        self.observed[cpu_id].load(Ordering::Acquire) >= self.generation.load(Ordering::Acquire)
    }

    /// 取得本轮页级失效必须使用的硬件 ASID。
    ///
    /// 有活跃 CPU 意味着该 MM 至少完成过一次 `activate_on()`，因此 LA64 必须
    /// 已拥有非零 ASID。调用者在 VM 锁内取值，使 ASID 与本轮 VPN 成为同一快照。
    #[cfg(target_arch = "loongarch64")]
    pub(crate) fn flush_asid(&self, targets: usize) -> u16 {
        if targets == 0 {
            return 0;
        }
        let asid = crate::hal::arch::loongarch64::tlb::hardware_asid(
            self.asid_context.load(Ordering::Acquire),
        );
        assert_ne!(asid, 0, "active LoongArch MM has no hardware ASID");
        asid
    }

    /// 返回与本轮 RV64 PTE 修改一起从同一个原子 context 冻结的硬件 ASID
    /// 和软件 epoch。PTE writer 必须比较这个 MM epoch，而不是另读全局 epoch。
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn flush_asid_context(&self, targets: usize) -> (u16, u64) {
        let context = self.asid_context.load(Ordering::Acquire);
        let asid = crate::hal::arch::riscv::sv39::hardware_asid(context);
        let epoch = context >> 16;
        if targets == 0 {
            return (0, epoch);
        }
        assert!(
            asid != 0 || crate::hal::arch::riscv::sv39::asid_capacity() == 0,
            "active RISC-V MM has no hardware ASID"
        );
        (asid, epoch)
    }

    /// 登记当前 CPU，并在使用页表根前完成首次 membarrier fence 与 TLB 代际追赶。
    ///
    /// 两项工作必须和 AddressSpace 的目标快照共用同一 VM 锁；拆开会重新引入
    /// “快照遗漏新 CPU，而新 CPU 又未执行完整屏障”的窗口。
    pub(crate) fn activate_cpu(&self, cpu_id: usize) -> bool {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        let mut caught_up = false;
        let cpu_bit = 1usize << cpu_id;
        let active = self.active_cpus.load(Ordering::Acquire);
        if active & cpu_bit == 0 {
            // 激活与修改都持有同一把地址空间锁；登记不会漏过修改方快照。
            let previous = self.active_cpus.fetch_or(cpu_bit, Ordering::AcqRel);
            if previous & cpu_bit == 0 {
                // 若本 CPU 在 PRIVATE_EXPEDITED 取目标快照后才首次进入该 MM，
                // VM 锁与这道 full fence 共同替代一次远端 IPI，保证返回用户态
                // 前经过 membarrier 所要求的有序点。
                fence(Ordering::SeqCst);
            }
        }

        loop {
            let generation = self.generation.load(Ordering::Acquire);
            let observed = self.observed[cpu_id].load(Ordering::Acquire);
            if observed < generation {
                caught_up = true;
                crate::hal::user_tlb_invalidate();
                self.observed[cpu_id].fetch_max(generation, Ordering::Release);
            }
            // 若修改方在本次 flush 后推进了代际，必须重新失效后才能使用页表根。
            if self.generation.load(Ordering::Acquire) == generation {
                return caught_up;
            }
        }
    }

    /// 当前 CPU 已切回 idle，清除它直接返回该 MM 的资格。
    ///
    /// 完整屏障必须先于 active bit 的 Release 清除。这样在同一 VM 锁后快照到
    /// bit 已清除的 membarrier/PTE writer，可以把本次切离当作有序点；后续
    /// 再次进入则必须经过 `activate_cpu()` 的 generation 追赶。
    pub(crate) fn deactivate_cpu(&self, cpu_id: usize) {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        let cpu_bit = 1usize << cpu_id;
        if self.active_cpus.load(Ordering::Acquire) & cpu_bit == 0 {
            return;
        }
        fence(Ordering::SeqCst);
        let previous = self.active_cpus.fetch_and(!cpu_bit, Ordering::AcqRel);
        assert_ne!(
            previous & cpu_bit,
            0,
            "active MM CPU bit disappeared while holding its address-space lock"
        );
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

    /// 指定 CPU 已完成本代失效；单调发布它真正观察到的 MM generation。
    pub(crate) fn mark_cpu_observed(&self, generation: usize, cpu_id: usize) {
        assert!(cpu_id < crate::smp::configured_cpu_count());
        self.observed[cpu_id].fetch_max(generation, Ordering::Release);
    }

    fn mark_targets_observed(&self, generation: usize, targets: usize) {
        for cpu_id in 0..crate::smp::configured_cpu_count() {
            if targets & (1usize << cpu_id) != 0 {
                // 同一 MM 的两代修改可能并发等待；旧一代晚完成时不能覆盖新值。
                self.mark_cpu_observed(generation, cpu_id);
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
        generation: usize,
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
        if self.targets != 0 {
            let current_bit = 1usize << crate::smp::cpu_id();
            let result = if self.targets == current_bit {
                match self.gather.range() {
                    FlushRange::None => panic!("TLB flush has no recorded PTE change"),
                    FlushRange::Range(range) => {
                        crate::hal::user_tlb_invalidate_range(self.asid, range)
                    }
                    FlushRange::Full => crate::hal::user_tlb_invalidate(),
                }
                Ok(())
            } else {
                let (range, mm_generation) = match self.gather.range() {
                    FlushRange::None => panic!("TLB flush has no recorded PTE change"),
                    // 只有 fixed range slot 会在目标 handler 内发布 observed；全刷仍由
                    // 发送方在同步返回后统一记账，不能携带精准槽专用的 MM 元数据。
                    FlushRange::Range(range) => {
                        (Some(range), Some((self.context, self.generation)))
                    }
                    FlushRange::Full => (None, None),
                };
                crate::smp::synchronize_user_tlb(self.targets, self.asid, range, mm_generation)
            };

            if let Err(error) = result {
                self.gather.leak_retired_frames();
                self.executed = true;
                panic!(
                    "user TLB shootdown failed: mm={} generation={} targets={:#x} error={:?}",
                    self.context.mm_id, self.generation, self.targets, error
                );
            }
            self.context
                .mark_targets_observed(self.generation, self.targets);
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
            "unexecuted user TLB flush: mm={} generation={} targets={:#x}",
            self.context.mm_id, self.generation, self.targets
        );
    }
}
