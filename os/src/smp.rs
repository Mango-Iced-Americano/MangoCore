//! BSP/AP 最小启动握手与 IPI mailbox。
//!
//! Phase 1 建立 CPU-local 状态和独立 idle stack；Phase 2 让 AP 响应无锁
//! IPI reason；Phase 3 在 BSP 发布 scheduler-ready 后让 AP 进入本地调度循环。

use core::{
    hint::spin_loop,
    mem::size_of,
    sync::atomic::{
        fence, AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering,
    },
};

pub const BOOT_CPU_ID: usize = 0;
pub const MAX_CPUS: usize = 8;
/// 精确 shootdown 在 hard IRQ 中可执行的最大连续页数。
///
/// 超过该跨度时上层改用全用户 TLB 失效，从而为软件 IPI
/// handler 提供确定的工作量上界。
pub(crate) const MAX_USER_TLB_RANGE_PAGES: usize = 64;

/// Phase 1 的 CPU-local 锚点；后续批次只扩展表项，不移动现有地址。
#[repr(C, align(64))]
struct PerCpu {
    /// 当前表项对应的 MangoCore 逻辑 CPU 编号，用于校验 CPU-local 指针归属。
    logical_id: usize,
    /// 本 CPU 独占的 current 槽和 idle 调度上下文。
    task_state: crate::task::processor::CpuTaskState,
    /// 本 CPU 是否已完成本地初始化；由所属 CPU Release 发布，其他 CPU Acquire 读取。
    online: AtomicBool,
    /// 本 CPU 是否已经切换到独立 idle stack；不表示此刻一定停在 idle 指令中。
    idle: AtomicBool,
    /// 本 CPU 是否已经越过 scheduler-ready 屏障并进入自己的调度循环。
    scheduler_entered: AtomicBool,
    /// RESCHEDULE IPI 发布的本地调度请求；handler 只置位，安全点负责消费。
    need_resched: AtomicBool,
    /// 本 CPU 在安全点实际消费的 RESCHEDULE 次数，供调度诊断与回归测试使用。
    reschedule_count: AtomicUsize,
    /// 目标 CPU 必须完成的 kernel-global 映射发布序号。
    kernel_tlb_request: AtomicUsize,
    /// 本 CPU 完成本地 TLB 刷新后发布的对应确认序号。
    kernel_tlb_ack: AtomicUsize,
    /// 目标 CPU 必须完成的全用户/non-global TLB 失效序号。
    user_tlb_request: AtomicUsize,
    /// 本 CPU 完成全用户 TLB 失效后发布的对应确认序号。
    user_tlb_ack: AtomicUsize,
    /// 本 CPU 发起的 kernel-global 全量远端 shootdown 轮数。
    tlb_kernel_full: AtomicUsize,
    /// 本 CPU 原本就要求全用户失效的远端 shootdown 轮数。
    tlb_user_full: AtomicUsize,
    /// 本 CPU 选择架构固件执行用户精准远端 shootdown 的轮数。
    tlb_user_range_firmware: AtomicUsize,
    /// 本 CPU 选择固定槽和 IPI 执行用户精准远端 shootdown 的轮数。
    tlb_user_range_ipi: AtomicUsize,
    /// 精准请求因本 CPU 的固定槽被占用而退化为全刷的轮数。
    tlb_user_range_fallback: AtomicUsize,
    /// 上述两类精准 shootdown 请求覆盖的总页数。
    tlb_user_range_pages: AtomicUsize,
    /// 本 CPU 所有远端 TLB 同步尝试覆盖的目标 CPU 数量总和。
    tlb_remote_targets: AtomicUsize,
    /// 本 CPU 远端 TLB 同步从选择后端到确认完成的累计原始 ticks。
    tlb_sync_ticks_total: AtomicUsize,
    /// 本 CPU 单轮远端 TLB 同步耗时的最大原始 ticks。
    tlb_sync_ticks_max: AtomicUsize,
    /// 本 CPU 收到错误返回的 TLB 同步轮数；doorbell 单点失败由 IPI 诊断统计。
    tlb_sync_failures: AtomicUsize,
    /// 本 CPU 必须执行的 membarrier 完整内存屏障序号。
    memory_barrier_request: AtomicUsize,
    /// 本 CPU 执行完整内存屏障后发布的对应确认序号。
    memory_barrier_ack: AtomicUsize,
    /// 尚未处理的 IPI 原因位图；发送方 Release 合并，目标 CPU Acquire 消费。
    pending_ipi: AtomicU32,
    /// 本 CPU 提交硬件 doorbell 失败的累计次数。
    ipi_send_failures: AtomicUsize,
    /// 本 CPU 进入 IPI hard handler 的次数；冗余 doorbell 也会计入。
    ipi_interrupts: AtomicUsize,
    /// 本 CPU 向目标 mailbox 发布各 reason 的次数，广播按目标数累计。
    ipi_reasons_published: [AtomicUsize; IPI_REASON_COUNT],
    /// 本 CPU 从 mailbox 实际消费各 reason bit 的次数；同类发布可被合并。
    ipi_reasons_consumed: [AtomicUsize; IPI_REASON_COUNT],
    /// hard IRQ 已收到 STOP；真正停止必须延后到 AP 独立 idle stack。
    stop_requested: AtomicBool,
    /// 本 CPU 已承诺不再访问共享内核状态，供 CPU0 等待停机完成。
    stopped: AtomicBool,
    /// 本 CPU 是否有尚未在安全点处理的 timer 工作；多个 IRQ 可以合并。
    timer_pending: AtomicBool,
    /// 本 CPU 下一次调度 tick 的绝对纳秒 deadline，只由所属 CPU 推进。
    sched_tick_deadline_ns: AtomicU64,
    /// AP 发布了更早的全局 timer；CPU0 在安全点读取队列并重编程本地硬件。
    timer_reprogram_requested: AtomicBool,
    /// 本 CPU 进入 timer 硬中断 fast path 的次数，仅用于诊断和 focused test。
    timer_irq_count: AtomicUsize,
    /// 本 CPU 在任务或 idle 安全点完成 timer 工作的批次数。
    timer_deferred_count: AtomicUsize,
}

impl PerCpu {
    const fn new(logical_id: usize) -> Self {
        Self {
            logical_id,
            task_state: crate::task::processor::CpuTaskState::new(),
            online: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            scheduler_entered: AtomicBool::new(false),
            need_resched: AtomicBool::new(false),
            reschedule_count: AtomicUsize::new(0),
            kernel_tlb_request: AtomicUsize::new(0),
            kernel_tlb_ack: AtomicUsize::new(0),
            user_tlb_request: AtomicUsize::new(0),
            user_tlb_ack: AtomicUsize::new(0),
            tlb_kernel_full: AtomicUsize::new(0),
            tlb_user_full: AtomicUsize::new(0),
            tlb_user_range_firmware: AtomicUsize::new(0),
            tlb_user_range_ipi: AtomicUsize::new(0),
            tlb_user_range_fallback: AtomicUsize::new(0),
            tlb_user_range_pages: AtomicUsize::new(0),
            tlb_remote_targets: AtomicUsize::new(0),
            tlb_sync_ticks_total: AtomicUsize::new(0),
            tlb_sync_ticks_max: AtomicUsize::new(0),
            tlb_sync_failures: AtomicUsize::new(0),
            memory_barrier_request: AtomicUsize::new(0),
            memory_barrier_ack: AtomicUsize::new(0),
            pending_ipi: AtomicU32::new(0),
            ipi_send_failures: AtomicUsize::new(0),
            ipi_interrupts: AtomicUsize::new(0),
            ipi_reasons_published: [const { AtomicUsize::new(0) }; IPI_REASON_COUNT],
            ipi_reasons_consumed: [const { AtomicUsize::new(0) }; IPI_REASON_COUNT],
            stop_requested: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            timer_pending: AtomicBool::new(false),
            sched_tick_deadline_ns: AtomicU64::new(0),
            timer_reprogram_requested: AtomicBool::new(false),
            timer_irq_count: AtomicUsize::new(0),
            timer_deferred_count: AtomicUsize::new(0),
        }
    }
}

/// 不依赖普通锁的单 CPU 诊断状态；各字段是 best-effort 快照。
pub(crate) struct CpuDiagnostics {
    pub(crate) cpu_id: usize,
    pub(crate) online: bool,
    pub(crate) idle_context_ready: bool,
    pub(crate) scheduler_entered: bool,
    pub(crate) stop_requested: bool,
    pub(crate) stopped: bool,
    pub(crate) need_resched: bool,
    pub(crate) pending_ipi: u32,
    pub(crate) timer_pending: bool,
    pub(crate) reschedule_count: usize,
    pub(crate) timer_irq_count: usize,
    pub(crate) timer_deferred_count: usize,
    pub(crate) kernel_tlb_request: usize,
    pub(crate) kernel_tlb_ack: usize,
    pub(crate) user_tlb_request: usize,
    pub(crate) user_tlb_ack: usize,
    pub(crate) tlb_kernel_full: usize,
    pub(crate) tlb_user_full: usize,
    pub(crate) tlb_user_range_firmware: usize,
    pub(crate) tlb_user_range_ipi: usize,
    pub(crate) tlb_user_range_fallback: usize,
    pub(crate) tlb_user_range_pages: usize,
    pub(crate) tlb_remote_targets: usize,
    pub(crate) tlb_sync_ticks_total: usize,
    pub(crate) tlb_sync_ticks_max: usize,
    pub(crate) tlb_sync_failures: usize,
    pub(crate) memory_barrier_request: usize,
    pub(crate) memory_barrier_ack: usize,
    pub(crate) ipi_interrupts: usize,
    pub(crate) ipi_send_failures: usize,
    pub(crate) ipi_reasons_published: [usize; IPI_REASON_COUNT],
    pub(crate) ipi_reasons_consumed: [usize; IPI_REASON_COUNT],
    pub(crate) task: crate::task::processor::CpuTaskDiagnostics,
}

/// 一次远端“ASID + 有界连续区间”失效的无锁共享槽。
///
/// 每个发起 CPU 固定拥有一个槽；当前安全点抢占模型保证同一 CPU 最多等待
/// 一轮同步。`claimed` 仍显式防御未来重入，重入时上层退回全用户 flush。
/// handler 只读原子字段并写 ack，不分配内存，也不获取任何普通锁。
struct UserTlbRangeSlot {
    claimed: AtomicBool,
    targets: AtomicUsize,
    acknowledged: AtomicUsize,
    asid: AtomicUsize,
    start_vpn: AtomicUsize,
    page_count: AtomicUsize,
    /// 指向同步等待期间保证存活的 MM TLB 状态；null 表示裸同步测试。
    context: AtomicPtr<crate::mm::TlbContext>,
    generation: AtomicUsize,
}

impl UserTlbRangeSlot {
    const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            targets: AtomicUsize::new(0),
            acknowledged: AtomicUsize::new(0),
            asid: AtomicUsize::new(0),
            start_vpn: AtomicUsize::new(0),
            page_count: AtomicUsize::new(0),
            context: AtomicPtr::new(core::ptr::null_mut()),
            generation: AtomicUsize::new(0),
        }
    }

    fn try_claim(&self) -> bool {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// 以 `targets` 的 Release store 作为整份 payload 的发布点。
    fn publish(
        &self,
        targets: usize,
        asid: u16,
        range: crate::mm::VPNRange,
        mm_generation: Option<(&crate::mm::TlbContext, usize)>,
    ) {
        debug_assert_ne!(targets, 0);
        let page_count = range.get_end().0 - range.get_start().0;
        debug_assert!((1..=MAX_USER_TLB_RANGE_PAGES).contains(&page_count));
        self.acknowledged.store(0, Ordering::Relaxed);
        self.asid.store(asid as usize, Ordering::Relaxed);
        self.start_vpn
            .store(range.get_start().0, Ordering::Relaxed);
        self.page_count.store(page_count, Ordering::Relaxed);
        let (context, generation) = mm_generation
            .map(|(context, generation)| (context as *const _ as *mut _, generation))
            .unwrap_or((core::ptr::null_mut(), 0));
        self.context.store(context, Ordering::Relaxed);
        self.generation.store(generation, Ordering::Relaxed);
        self.targets.store(targets, Ordering::Release);
    }

    /// 当前 CPU 在 hard IRQ 中处理属于自己的 payload，并在指令完成后 ack。
    fn service(&self, cpu_id: usize) {
        let cpu_bit = 1usize << cpu_id;
        if self.targets.load(Ordering::Acquire) & cpu_bit == 0
            || self.acknowledged.load(Ordering::Acquire) & cpu_bit != 0
        {
            return;
        }
        let asid = self.asid.load(Ordering::Relaxed) as u16;
        let start = self.start_vpn.load(Ordering::Relaxed);
        let page_count = self.page_count.load(Ordering::Relaxed);
        let end = start
            .checked_add(page_count)
            .expect("published user TLB range overflowed");
        let range = crate::mm::VPNRange::new(start.into(), end.into());
        crate::hal::user_tlb_invalidate_range(asid, range);
        let context = self.context.load(Ordering::Relaxed);
        if !context.is_null() {
            let generation = self.generation.load(Ordering::Relaxed);
            // Safety: 发起者持有借用 `TlbContext` 的 `TlbFlush`，并且只有在
            // 观察到下方 ack 后才会返回或释放槽。因此 handler 完成本次访问前，
            // `context` 必定仍然存活；fail-stop 超时路径也不会复用该槽。
            unsafe { &*context }.mark_cpu_observed(generation, cpu_id);
        }
        self.acknowledged.fetch_or(cpu_bit, Ordering::Release);
    }

    fn acknowledged(&self) -> usize {
        self.acknowledged.load(Ordering::Acquire)
    }

    /// 只有发起者确认全部 live target 已 ack 后才能复用该槽。
    fn release(&self) {
        self.targets.store(0, Ordering::Release);
        self.claimed.store(false, Ordering::Release);
    }
}

static USER_TLB_RANGE_SLOTS: [UserTlbRangeSlot; MAX_CPUS] =
    [const { UserTlbRangeSlot::new() }; MAX_CPUS];

/// 可以合并进 per-CPU mailbox 的幂等 IPI 原因。
///
/// reason bit 只表示“至少处理一次”，不能表示事件次数；需要计数的协议必须
/// 另外使用 sequence/ack，并在复用同一 bit 前等待前一轮完成。
#[derive(Clone, Copy)]
pub struct IpiReason(u32);

impl IpiReason {
    /// 请求 AP 在退出 hard IRQ 后停止，不再访问任何共享内核状态。
    const STOP: Self = Self(1 << 0);
    /// 目标 runqueue 已加入任务；handler 只发布 need-resched。
    const RESCHEDULE: Self = Self(1 << 1);
    /// BSP 已修改共享内核页表；目标必须刷新本地 TLB 后发布 ack。
    const KERNEL_TLB_SYNC: Self = Self(1 << 2);
    /// 某个用户 MM 的 PTE 已修改；目标必须清除本核全部用户翻译后 ack。
    const USER_TLB_SYNC: Self = Self(1 << 3);
    /// 固定槽中已发布目标 MM 的 ASID/VPN 区间；目标精确失效后 ack。
    const USER_TLB_RANGE_SYNC: Self = Self(1 << 4);
    /// 全局 timer 队列出现更早 deadline；CPU0 在安全点重编程本地 timer。
    const TIMER_REPROGRAM: Self = Self(1 << 5);
    /// 目标 CPU 必须执行完整内存屏障后发布 ack。
    const MEMORY_BARRIER: Self = Self(1 << 6);

    const fn bits(self) -> u32 {
        self.0
    }
}

const IPI_REASON_COUNT: usize = IpiReason::MEMORY_BARRIER.bits().trailing_zeros() as usize + 1;

pub(crate) const IPI_REASON_NAMES: [&str; IPI_REASON_COUNT] = [
    "stop",
    "reschedule",
    "kernel-tlb",
    "user-tlb",
    "user-tlb-range",
    "timer-reprogram",
    "membarrier",
];

/// 记录一份 mailbox 位图中的已知 reason；诊断不能因异常位破坏 IPI 处理。
fn record_ipi_reasons(counters: &[AtomicUsize; IPI_REASON_COUNT], reasons: u32, count: usize) {
    // 只扫描已有名字的低位，既保持 hard IRQ 工作量固定，也让未来新增 reason
    // 即使漏补诊断映射也只少计一次，而不会把生产 IPI 路径变成 panic 点。
    let mut reasons = reasons & ((1u32 << IPI_REASON_COUNT) - 1);
    while reasons != 0 {
        let index = reasons.trailing_zeros() as usize;
        counters[index].fetch_add(count, Ordering::Relaxed);
        reasons &= reasons - 1;
    }
}

// 显式放入 `.data.boot`，保证 LA64 AP 在 CPU0 清 BSS 前就能安全取得自己的
// cache-line 表项；运行期只允许通过表项内部的原子字段修改状态。
#[link_section = ".data.boot"]
static PER_CPUS: [PerCpu; MAX_CPUS] = [
    PerCpu::new(0),
    PerCpu::new(1),
    PerCpu::new(2),
    PerCpu::new(3),
    PerCpu::new(4),
    PerCpu::new(5),
    PerCpu::new(6),
    PerCpu::new(7),
];

// build.rs 会拒绝除单字节字符串 1/2/4/8 之外的构建参数。
const CONFIGURED_CPU_COUNT: usize = (env!("MANGO_CORE_NUM").as_bytes()[0] - b'0') as usize;

const AP_RELEASED: usize = 1;
const ONLINE_TIMEOUT_SECONDS: usize = 5;
const STOP_TIMEOUT_SECONDS: usize = 1;
// Kernel-global mapping publication may run while an AP is handling a long
// scheduler/idle transition.  Keep the short stop/membarrier deadline for
// those protocols, but give this sequence/ack protocol its own budget so a
// slow emulated vCPU is not mistaken for a lost mapping publication.
const KERNEL_TLB_TIMEOUT_SECONDS: usize = 5;
const UNCLAIMED_BOOT_HARDWARE_ID: usize = usize::MAX;

/// CPU0 等待 AP 停止时唯一可能返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopError {
    /// 目前只有 CPU0 拥有最终停机协议；AP panic 走 HAL 的机器级兜底。
    NotBootCpu { cpu_id: usize },
    /// 有 AP 未在期限内发布 stopped；同时保留 doorbell 的首个发送错误。
    Timeout {
        missing: usize,
        send_error: Option<isize>,
    },
}

/// kernel-global 映射发布或撤销时可能发生的同步错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KernelTlbSyncError {
    InvalidCpu { cpu_id: usize },
    InvalidTargets { targets: usize },
    UnavailableTargets { targets: usize, available: usize },
    Timeout {
        cpu_id: usize,
        expected: usize,
        observed: usize,
        send_error: Option<isize>,
    },
}

/// 全用户 TLB 同步基础设施可能返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserTlbSyncError {
    InvalidTargets { targets: usize },
    UnavailableTargets { targets: usize, available: usize },
    InvalidRange { start_vpn: usize, end_vpn: usize },
    Firmware { error: isize },
    Timeout {
        cpu_id: usize,
        expected: usize,
        observed: usize,
        send_error: Option<isize>,
    },
    RangeTimeout {
        missing: usize,
        acknowledged: usize,
        send_error: Option<isize>,
    },
}

/// 一轮远端 TLB 同步最终选择的实际执行后端。
///
/// 五个分支互斥，避免把“原生全刷”和“精准请求退化为全刷”混在同一计数里。
#[derive(Clone, Copy)]
enum TlbShootdownKind {
    KernelFull,
    UserFull,
    UserRangeFirmware { pages: usize },
    UserRangeIpi { pages: usize },
    UserRangeFallback,
}

/// 在发起 CPU 记录一轮远端 TLB 同步的最终结果。
///
/// 该函数只更新 Relaxed 诊断值，不参与 request/ack、generation 或 frame 退休同步。
fn record_tlb_shootdown(
    kind: TlbShootdownKind,
    remote_targets: usize,
    started_at: usize,
    failed: bool,
) {
    debug_assert_ne!(remote_targets, 0);
    let local = &PER_CPUS[self::cpu_id()];
    match kind {
        TlbShootdownKind::KernelFull => &local.tlb_kernel_full,
        TlbShootdownKind::UserFull => &local.tlb_user_full,
        TlbShootdownKind::UserRangeFirmware { pages } => {
            local
                .tlb_user_range_pages
                .fetch_add(pages, Ordering::Relaxed);
            &local.tlb_user_range_firmware
        }
        TlbShootdownKind::UserRangeIpi { pages } => {
            local
                .tlb_user_range_pages
                .fetch_add(pages, Ordering::Relaxed);
            &local.tlb_user_range_ipi
        }
        TlbShootdownKind::UserRangeFallback => &local.tlb_user_range_fallback,
    }
    .fetch_add(1, Ordering::Relaxed);

    local
        .tlb_remote_targets
        .fetch_add(remote_targets.count_ones() as usize, Ordering::Relaxed);
    let elapsed = crate::hal::get_time().wrapping_sub(started_at);
    local
        .tlb_sync_ticks_total
        .fetch_add(elapsed, Ordering::Relaxed);
    local
        .tlb_sync_ticks_max
        .fetch_max(elapsed, Ordering::Relaxed);
    if failed {
        local.tlb_sync_failures.fetch_add(1, Ordering::Relaxed);
    }
}

/// 跨 CPU 完整内存屏障协议可能返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemoryBarrierError {
    InvalidTargets {
        targets: usize,
    },
    UnavailableTargets {
        targets: usize,
        available: usize,
    },
    Timeout {
        cpu_id: usize,
        expected: usize,
        observed: usize,
        send_error: Option<isize>,
    },
}

// These values must survive CPU0's BSS clear while LA64 APs are already
// polling them on their private boot stacks.
#[link_section = ".data.boot"]
static BOOT_HARDWARE_ID: AtomicUsize = AtomicUsize::new(UNCLAIMED_BOOT_HARDWARE_ID);
#[link_section = ".data.boot"]
static BOOT_PHASE: AtomicUsize = AtomicUsize::new(0);
/// BSP 完成任务所依赖的全局初始化后发布；AP Acquire 后才能进入调度器。
#[link_section = ".data.boot"]
static SCHEDULER_RELEASED: AtomicBool = AtomicBool::new(false);

const fn expected_online_mask() -> usize {
    (1usize << CONFIGURED_CPU_COUNT) - 1
}

/// 返回本次构建配置的逻辑 CPU 数量。
pub const fn configured_cpu_count() -> usize {
    CONFIGURED_CPU_COUNT
}

/// 汇总当前已经完成本地初始化的 CPU。
///
/// online 在 Phase 1 中只会由对应 CPU 从 false 发布为 true，因此逐项
/// Acquire 扫描即使看到中间快照也不会丢失已经观察到的发布状态。读到 true
/// 同时保证该 CPU 在 Release 前完成的本地初始化对当前 CPU 可见。
pub fn online_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if PER_CPUS[cpu_id].online.load(Ordering::Acquire) {
            mask |= 1usize << cpu_id;
        }
    }
    mask
}

/// 汇总已经进入 idle 执行上下文的 CPU。
///
/// AP 切到独立 idle stack 后只置位一次。当前字段表示执行上下文所有权，
/// 并不随每次 `wfi`/`idle` 睡眠清零；后续调度器会另行补全瞬时 idle 握手。
pub fn idle_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if PER_CPUS[cpu_id].idle.load(Ordering::Acquire) {
            mask |= 1usize << cpu_id;
        }
    }
    mask
}

/// 汇总已经进入 per-CPU 调度循环的 CPU。
pub fn scheduler_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if PER_CPUS[cpu_id]
            .scheduler_entered
            .load(Ordering::Acquire)
        {
            mask |= 1usize << cpu_id;
        }
    }
    mask
}

/// 汇总已经进入不可返回 stop loop 的 AP。
pub fn stopped_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if PER_CPUS[cpu_id].stopped.load(Ordering::Acquire) {
            mask |= 1usize << cpu_id;
        }
    }
    mask
}

/// 读取 panic/STOP 路径可安全打印的指定 CPU 状态。
///
/// 该函数不等待任何普通锁；任务侧唯一的锁访问是 `active_user_vm.try_lock()`。
/// 远端 CPU 可能继续改变状态，因此结果只用于诊断，不能参与调度或资源释放决策。
pub(crate) fn cpu_diagnostics(cpu_id: usize) -> CpuDiagnostics {
    assert!(cpu_id < CONFIGURED_CPU_COUNT);
    let cpu = &PER_CPUS[cpu_id];
    CpuDiagnostics {
        cpu_id,
        online: cpu.online.load(Ordering::Acquire),
        idle_context_ready: cpu.idle.load(Ordering::Acquire),
        scheduler_entered: cpu.scheduler_entered.load(Ordering::Acquire),
        stop_requested: cpu.stop_requested.load(Ordering::Acquire),
        stopped: cpu.stopped.load(Ordering::Acquire),
        need_resched: cpu.need_resched.load(Ordering::Acquire),
        pending_ipi: cpu.pending_ipi.load(Ordering::Acquire),
        timer_pending: cpu.timer_pending.load(Ordering::Acquire),
        reschedule_count: cpu.reschedule_count.load(Ordering::Relaxed),
        timer_irq_count: cpu.timer_irq_count.load(Ordering::Relaxed),
        timer_deferred_count: cpu.timer_deferred_count.load(Ordering::Relaxed),
        kernel_tlb_request: cpu.kernel_tlb_request.load(Ordering::Acquire),
        kernel_tlb_ack: cpu.kernel_tlb_ack.load(Ordering::Acquire),
        user_tlb_request: cpu.user_tlb_request.load(Ordering::Acquire),
        user_tlb_ack: cpu.user_tlb_ack.load(Ordering::Acquire),
        tlb_kernel_full: cpu.tlb_kernel_full.load(Ordering::Relaxed),
        tlb_user_full: cpu.tlb_user_full.load(Ordering::Relaxed),
        tlb_user_range_firmware: cpu.tlb_user_range_firmware.load(Ordering::Relaxed),
        tlb_user_range_ipi: cpu.tlb_user_range_ipi.load(Ordering::Relaxed),
        tlb_user_range_fallback: cpu.tlb_user_range_fallback.load(Ordering::Relaxed),
        tlb_user_range_pages: cpu.tlb_user_range_pages.load(Ordering::Relaxed),
        tlb_remote_targets: cpu.tlb_remote_targets.load(Ordering::Relaxed),
        tlb_sync_ticks_total: cpu.tlb_sync_ticks_total.load(Ordering::Relaxed),
        tlb_sync_ticks_max: cpu.tlb_sync_ticks_max.load(Ordering::Relaxed),
        tlb_sync_failures: cpu.tlb_sync_failures.load(Ordering::Relaxed),
        memory_barrier_request: cpu.memory_barrier_request.load(Ordering::Acquire),
        memory_barrier_ack: cpu.memory_barrier_ack.load(Ordering::Acquire),
        ipi_interrupts: cpu.ipi_interrupts.load(Ordering::Relaxed),
        ipi_send_failures: cpu.ipi_send_failures.load(Ordering::Relaxed),
        ipi_reasons_published: core::array::from_fn(|index| {
            cpu.ipi_reasons_published[index].load(Ordering::Relaxed)
        }),
        ipi_reasons_consumed: core::array::from_fn(|index| {
            cpu.ipi_reasons_consumed[index].load(Ordering::Relaxed)
        }),
        task: cpu.task_state.read_diagnostics(),
    }
}

/// 向一组 online CPU 发布同一个幂等 reason，再逐个触发硬件 doorbell。
///
/// 所有 mailbox 都先完成 Release 发布，目标 CPU 才可能开始处理。若某个
/// doorbell 失败，已经发布的 reason 保留到后续 IPI 消费，不能回滚原子状态。
pub fn send_ipi_mask(targets: usize, reason: IpiReason) -> Result<(), isize> {
    let configured = expected_online_mask();
    let sender = self::cpu_id();
    if reason.bits() == 0 || targets & !configured != 0 {
        return Err(-3);
    }
    if targets & (1usize << sender) != 0 {
        return Err(-3);
    }
    if targets & !online_cpu_mask() != 0 {
        return Err(-4);
    }

    // 先向全部目标 Release 发布 reason，再允许任一目标开始处理。这为整轮
    // 广播建立统一的“publication before delivery”批次边界；handler 仍然
    // 只读取本 CPU mailbox，不依赖其他 CPU 的状态。
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if targets & (1usize << cpu_id) != 0 {
            PER_CPUS[cpu_id]
                .pending_ipi
                .fetch_or(reason.bits(), Ordering::Release);
        }
    }
    // IpiReason 的公开构造只有单 bit 常量。publication 按目标数计数；
    // 同类 reason 若在接收前重复发布，mailbox 会合并，所以不能用
    // published-consumed 的差值直接判断丢中断。
    debug_assert_eq!(reason.bits().count_ones(), 1);
    record_ipi_reasons(
        &PER_CPUS[sender].ipi_reasons_published,
        reason.bits(),
        targets.count_ones() as usize,
    );

    let boot_hardware_id = BOOT_HARDWARE_ID.load(Ordering::Acquire);
    let mut first_error = None;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if targets & (1usize << cpu_id) != 0 {
            let hardware_id = logical_to_hardware_id(cpu_id, boot_hardware_id);
            // 一个 doorbell 失败不能阻止其余已发布 mailbox 的目标被唤醒；
            // 完成整轮发送后再返回首个错误，失败目标的 reason 留待后续 IPI。
            if let Err(error) = crate::hal::send_ipi(hardware_id) {
                // 该计数只用于事后诊断，不承载 mailbox 或 ack 的同步关系。
                PER_CPUS[sender]
                    .ipi_send_failures
                    .fetch_add(1, Ordering::Relaxed);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// 向一个逻辑 CPU 发布通用 IPI reason。
pub fn send_ipi(cpu_id: usize, reason: IpiReason) -> Result<(), isize> {
    if cpu_id >= CONFIGURED_CPU_COUNT {
        return Err(-3);
    }
    send_ipi_mask(1usize << cpu_id, reason)
}

/// 查询目标 CPU 已完成的 kernel-global TLB 同步序号。
///
/// 该值只用于诊断和 focused test；资源生命周期必须由同步入口的等待结果
/// 决定，调用方不能自行比较一次快照后释放 frame。
pub(crate) fn kernel_tlb_ack(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].kernel_tlb_ack.load(Ordering::Acquire)
}

/// 查询目标 CPU 已完成的全用户 TLB 同步序号。
pub(crate) fn user_tlb_ack(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].user_tlb_ack.load(Ordering::Acquire)
}

/// 查询目标 CPU 已发布但可能尚未确认的全用户 TLB 请求序号。
///
/// 仅供诊断与生命周期 focused test 观察“request 已发布、ack 尚未完成”的
/// 窗口；生产释放路径仍必须调用同步入口，不能自行比较计数器。
pub(crate) fn user_tlb_request(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].user_tlb_request.load(Ordering::Acquire)
}

/// 查询目标 CPU 已完成的跨核完整内存屏障序号。
pub(crate) fn memory_barrier_ack(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].memory_barrier_ack.load(Ordering::Acquire)
}

/// 查询目标 CPU 已发布的跨核完整内存屏障序号。
pub(crate) fn memory_barrier_request(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id]
        .memory_barrier_request
        .load(Ordering::Acquire)
}

/// 查询目标 CPU 提交硬件 doorbell 失败的累计次数。
pub fn ipi_send_failures(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id]
        .ipi_send_failures
        .load(Ordering::Relaxed)
}

/// 让所有 online AP 停止，并等待它们承诺不再访问共享状态。
///
/// STOP 是终态而不是 CPU hotplug：`online` 保留启动历史，重复调用通过
/// `stopped` mask 幂等完成。即使部分 doorbell 报错，也继续等待；若目标
/// 因已有 pending interrupt 消费了 mailbox，最终结果仍以 stopped ack 为准。
pub fn stop_secondary_cpus() -> Result<(), StopError> {
    let caller = self::cpu_id();
    if caller != BOOT_CPU_ID {
        return Err(StopError::NotBootCpu { cpu_id: caller });
    }

    let targets = online_cpu_mask()
        & !(1usize << BOOT_CPU_ID)
        & !stopped_cpu_mask();
    if targets == 0 {
        return Ok(());
    }

    let send_error = send_ipi_mask(targets, IpiReason::STOP).err();
    let deadline = crate::hal::get_time().saturating_add(
        crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS),
    );
    loop {
        let missing = targets & !stopped_cpu_mask();
        if missing == 0 {
            return Ok(());
        }
        if crate::hal::get_time() >= deadline {
            return Err(StopError::Timeout {
                missing,
                send_error,
            });
        }
        spin_loop();
    }
}

/// 在当前 CPU 的硬中断上下文消费 mailbox。
///
/// handler 只做原子操作，不分配、不打印、不获取普通锁，也不直接调度。
pub fn handle_ipi() {
    let local = &PER_CPUS[self::cpu_id()];
    local.ipi_interrupts.fetch_add(1, Ordering::Relaxed);
    // Acquire 获取发送端在 Release fetch_or 前发布的数据；swap(0) 使原因只被
    // 当前 CPU 消费一次。doorbell 合并或重复到达都不会重复生成 ack。
    let reasons = local.pending_ipi.swap(0, Ordering::Acquire);
    record_ipi_reasons(&local.ipi_reasons_consumed, reasons, 1);
    if reasons & IpiReason::STOP.bits() != 0 {
        // 不可返回的 stop 不能发生在 trap frame 上；只向 idle 栈发布请求。
        local.stop_requested.store(true, Ordering::Release);
    }
    if reasons & IpiReason::RESCHEDULE.bits() != 0 {
        // runnable 已在发送方释放 runqueue 锁前完成发布；这里只留下无锁提示。
        local.need_resched.store(true, Ordering::Release);
    }
    if reasons & IpiReason::TIMER_REPROGRAM.bits() != 0 {
        // 发送方已经先发布标志；handler 再次置位使纯 mailbox 消费也保持幂等。
        // 读取全局 timer 队列需要普通锁，必须留到 CPU0 的任务/idle 安全点。
        local
            .timer_reprogram_requested
            .store(true, Ordering::Release);
    }
    if reasons & IpiReason::MEMORY_BARRIER.bits() != 0 {
        let sequence = local.memory_barrier_request.load(Ordering::Acquire);
        if local.memory_barrier_ack.load(Ordering::Acquire) < sequence {
            // 目标必须先经过完整硬件屏障，再允许发送方从 ack 等待中返回。
            fence(Ordering::SeqCst);
            local.memory_barrier_ack.store(sequence, Ordering::Release);
        }
    }
    if reasons & IpiReason::KERNEL_TLB_SYNC.bits() != 0 {
        // 必须先快照 request，再做失效。若反过来，发送方可能在“失效完成”
        // 与“读取 request”之间发布新序号，handler 就会错误地用旧 flush
        // 确认新请求，导致撤映射 frame 在目标仍持有旧 TLB 时提前释放。
        let sequence = local.kernel_tlb_request.load(Ordering::Acquire);
        crate::hal::kernel_tlb_invalidate();
        local.kernel_tlb_ack.store(sequence, Ordering::Release);
    }
    if reasons & IpiReason::USER_TLB_SYNC.bits() != 0 {
        // 用户协议拥有独立 sequence，不能和并发的 kernel-global 撤映射互相
        // 覆盖。先读 request 再 flush，确保 ack 对应的失效发生在请求发布之后。
        let sequence = local.user_tlb_request.load(Ordering::Acquire);
        if local.user_tlb_ack.load(Ordering::Acquire) < sequence {
            // 并发发布可能在 handler 清空 mailbox 后留下一个迟到
            // reason。若较新 sequence 已由上一次全刷确认，再刷一次
            // 只会增加 TLB refill 成本，不能提供更强的正确性。
            crate::hal::user_tlb_invalidate();
            local.user_tlb_ack.store(sequence, Ordering::Release);
        }
    }
    if reasons & IpiReason::USER_TLB_RANGE_SYNC.bits() != 0 {
        // 多个发起者可以共享同一个 reason bit；扫描固定的每 CPU 槽即可一次
        // 消费全部已发布 payload。每个槽自己的 target/ack 防止相互覆盖。
        let cpu_id = self::cpu_id();
        for slot in &USER_TLB_RANGE_SLOTS {
            slot.service(cpu_id);
        }
    }
}

/// 在 AP idle 栈上执行 hard IPI 延迟下来的有界工作。
///
/// 调用方已关闭本地全局中断，因此 pending 检查与下一次 wait 之间不存在
/// handler 插入窗口。函数不获取普通锁，也不进入调度器。
pub(crate) fn service_secondary_ipi_work() -> bool {
    let cpu_id = self::cpu_id();
    debug_assert_ne!(cpu_id, BOOT_CPU_ID);
    let local = &PER_CPUS[cpu_id];

    // STOP 优先于其他 deferred work。Release ack 承诺此前的 handler 状态
    // 已可见。先关闭本地 interrupt source，再发布 ack；CPU0 观察到 stopped
    // 后，AP 只剩发散 idle 指令，不会再被新 doorbell 唤醒或访问共享状态。
    if local.stop_requested.swap(false, Ordering::Acquire) {
        crate::hal::prepare_secondary_cpu_stop();
        local.stopped.store(true, Ordering::Release);
        crate::hal::secondary_cpu_stop();
    }

    take_reschedule_request()
}

/// 在当前 CPU 的关中断安全点取走一次可合并的调度请求。
///
/// hard IPI 以 Release 发布 `need_resched`，这里的 Acquire 使后续调度观察到
/// handler 之前的 mailbox 处理。调用方必须已经保存完整任务现场，或正运行在
/// idle 栈；函数只消费提示，不获取 runqueue 锁，也不直接切换任务。
pub(crate) fn take_reschedule_request() -> bool {
    let local = &PER_CPUS[self::cpu_id()];
    if !local.need_resched.swap(false, Ordering::Acquire) {
        return false;
    }
    local.reschedule_count.fetch_add(1, Ordering::Relaxed);
    true
}

/// 查询指定 CPU 已在安全点消费的 RESCHEDULE 次数。
pub(crate) fn reschedule_count(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id]
        .reschedule_count
        .load(Ordering::Relaxed)
}

/// 远程 runqueue 发布完成后唤醒目标 CPU。
pub(crate) fn request_reschedule(cpu_id: usize) -> Result<(), isize> {
    send_ipi(cpu_id, IpiReason::RESCHEDULE)
}

/// IPI ack 等待期间临时开放本地中断，并在退出时恢复调用者原状态。
///
/// 当前发起者可能同时成为另一轮同步的目标；若双方都在 IRQ-off
/// 自旋，就会互相等待 ack。进入本 guard 前不得持有页表、runqueue 或普通锁。
/// 窗口内到达的 timer IRQ 仍只发布 deferred work；生产调用者随后必须经过
/// trap-return 或 scheduler timer 安全点，不能在 MM 同步层执行任意 timer callback。
struct IpiWaitIrqGuard {
    restore_enabled: bool,
}

impl IpiWaitIrqGuard {
    fn enter() -> Self {
        let restore_enabled = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(true);
        Self { restore_enabled }
    }
}

impl Drop for IpiWaitIrqGuard {
    fn drop(&mut self) {
        let _ = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(self.restore_enabled);
    }
}

/// 让目标 CPU 在本次调用期间各自经过一次完整内存屏障。
///
/// 调用方不得持有 VM、runqueue 或其它普通锁。目标若在等待期间完成 STOP，
/// 其不可恢复的停止确认可替代 barrier ack；MangoCore 不支持 CPU hotplug。
pub(crate) fn synchronize_memory(targets: usize) -> Result<(), MemoryBarrierError> {
    let current_bit = 1usize << self::cpu_id();
    let targets = targets | current_bit;
    let configured = expected_online_mask();
    if targets & !configured != 0 {
        return Err(MemoryBarrierError::InvalidTargets { targets });
    }
    let online = online_cpu_mask();
    if targets & !online != 0 {
        return Err(MemoryBarrierError::UnavailableTargets {
            targets,
            available: online & !stopped_cpu_mask(),
        });
    }

    let live_targets = targets & !stopped_cpu_mask();
    let remote = live_targets & !current_bit;
    let mut expected = [0usize; MAX_CPUS];

    // 先约束调用者在 syscall 入口前的用户内存访问，再发布远端 request。
    fence(Ordering::SeqCst);
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if remote & (1usize << cpu_id) == 0 {
            continue;
        }
        expected[cpu_id] = PER_CPUS[cpu_id]
            .memory_barrier_request
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        assert_ne!(
            expected[cpu_id], 0,
            "memory barrier synchronization sequence wrapped"
        );
    }

    if remote != 0 {
        let send_error = send_ipi_mask(remote, IpiReason::MEMORY_BARRIER).err();
        let _irq_guard = IpiWaitIrqGuard::enter();
        let deadline = crate::hal::get_time()
            .saturating_add(crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS));
        loop {
            let stopped = stopped_cpu_mask();
            let mut missing = None;
            for cpu_id in 0..CONFIGURED_CPU_COUNT {
                if remote & (1usize << cpu_id) == 0 || stopped & (1usize << cpu_id) != 0 {
                    continue;
                }
                let observed = PER_CPUS[cpu_id].memory_barrier_ack.load(Ordering::Acquire);
                if observed < expected[cpu_id] {
                    missing = Some((cpu_id, observed));
                    break;
                }
            }
            let Some((cpu_id, observed)) = missing else {
                break;
            };
            if crate::hal::get_time() >= deadline {
                return Err(MemoryBarrierError::Timeout {
                    cpu_id,
                    expected: expected[cpu_id],
                    observed,
                    send_error,
                });
            }
            spin_loop();
        }
    }

    // ack 的 Acquire 与这道屏障共同约束 syscall 返回后的用户内存访问。
    fence(Ordering::SeqCst);
    Ok(())
}

/// 把一次共享内核页表修改同步到目标 CPU 集合。
///
/// 调用方必须已经释放 `KERNEL_SPACE` 以及其它普通锁，并把被撤映射资源保留到
/// 本函数成功返回。每个目标先取得独立 request 序号，再统一发布 reason/doorbell；
/// handler 对 request 的 Acquire 快照发生在 flush 之前，因此 ack 只覆盖真正
/// 完成失效的序号。
fn synchronize_kernel_mapping_mask(
    targets: usize,
    stopped_is_ack: bool,
) -> Result<(), KernelTlbSyncError> {
    let configured = expected_online_mask();
    if targets == 0 || targets & !configured != 0 {
        return Err(KernelTlbSyncError::InvalidTargets { targets });
    }
    let online = online_cpu_mask();
    let mut available = online & !stopped_cpu_mask();
    if targets & !online != 0 || (!stopped_is_ack && targets & !available != 0) {
        return Err(KernelTlbSyncError::UnavailableTargets { targets, available });
    }

    // 撤映射时 stopped ack 承诺目标不再访问共享状态，可以等价于 TLB ack；
    // 新映射发布则不能把“已停止”当成功，否则随后仍会向该 CPU 入队任务。
    let targets = if stopped_is_ack {
        targets & available
    } else {
        targets
    };
    if targets == 0 {
        return Ok(());
    }

    let current_bit = 1usize << self::cpu_id();
    let remote = targets & !current_bit;
    let mut expected = [0usize; MAX_CPUS];
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if remote & (1usize << cpu_id) == 0 {
            continue;
        }
        expected[cpu_id] = PER_CPUS[cpu_id]
            .kernel_tlb_request
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        assert_ne!(expected[cpu_id], 0, "kernel TLB sync sequence wrapped");
    }

    if targets & current_bit != 0 {
        crate::hal::kernel_tlb_invalidate();
    }
    if remote == 0 {
        return Ok(());
    }
    // 即使某个 doorbell 报错，也先等待已经发布的 mailbox：目标可能由另一
    // 个 pending interrupt 唤醒；若它同时完成 STOP，stopped ack 本身已经
    // 承诺不再访问旧翻译，也可替代本轮 TLB ack。
    let started_at = crate::hal::get_time();
    let mut send_error = send_ipi_mask(remote, IpiReason::KERNEL_TLB_SYNC).err();

    let _irq_guard = IpiWaitIrqGuard::enter();
    let clock_freq = crate::hal::get_clock_freq();
    let deadline = crate::hal::get_time()
        .saturating_add(clock_freq.saturating_mul(KERNEL_TLB_TIMEOUT_SECONDS));
    // The mailbox reason is level/sequence based and therefore idempotent:
    // re-issuing the hardware doorbell cannot duplicate a request, while it
    // repairs a one-shot IPI edge that was coalesced or delayed by QEMU TCG.
    let kick_interval = (clock_freq / 100).max(1);
    let mut next_kick = crate::hal::get_time().saturating_add(kick_interval);
    let result = 'wait: loop {
        let mut missing = None;
        let stopped = stopped_cpu_mask();
        available &= !stopped;
        for cpu_id in 0..CONFIGURED_CPU_COUNT {
            if remote & (1usize << cpu_id) == 0 {
                continue;
            }
            if stopped & (1usize << cpu_id) != 0 {
                if stopped_is_ack {
                    continue;
                }
                break 'wait Err(KernelTlbSyncError::UnavailableTargets {
                    targets: remote,
                    available,
                });
            }
            let observed = PER_CPUS[cpu_id].kernel_tlb_ack.load(Ordering::Acquire);
            if observed < expected[cpu_id] {
                missing = Some((cpu_id, observed));
                break;
            }
        }
        let Some((cpu_id, observed)) = missing else {
            break 'wait Ok(());
        };
        let now = crate::hal::get_time();
        if now >= next_kick {
            // Keep the original mailbox publication and sequence unchanged;
            // only retrigger delivery for CPUs that still need an ack.
            let kick_targets = remote & !stopped;
            if kick_targets != 0 {
                if let Err(error) = send_ipi_mask(kick_targets, IpiReason::KERNEL_TLB_SYNC) {
                    if send_error.is_none() {
                        send_error = Some(error);
                    }
                }
            }
            next_kick = now.saturating_add(kick_interval);
        }
        if now >= deadline {
            break 'wait Err(KernelTlbSyncError::Timeout {
                cpu_id,
                expected: expected[cpu_id],
                observed,
                send_error,
            });
        }
        spin_loop();
    };
    record_tlb_shootdown(
        TlbShootdownKind::KernelFull,
        remote,
        started_at,
        result.is_err(),
    );
    result
}

/// 在任务入队前，把新建的 kernel-global 映射同步到指定 CPU。
pub(crate) fn synchronize_kernel_mapping(cpu_id: usize) -> Result<(), KernelTlbSyncError> {
    if cpu_id >= CONFIGURED_CPU_COUNT {
        return Err(KernelTlbSyncError::InvalidCpu { cpu_id });
    }
    synchronize_kernel_mapping_mask(1usize << cpu_id, false)
}

/// 撤销共享内核映射后，使所有仍可能执行内核代码的 CPU 完成失效。
pub(crate) fn synchronize_kernel_mapping_all() -> Result<(), KernelTlbSyncError> {
    let targets = online_cpu_mask() & !stopped_cpu_mask();
    synchronize_kernel_mapping_mask(targets, true)
}

/// 让调用方选定的 CPU 集合同步完成用户 TLB 失效。
///
/// `range=Some(vpns)` 时先尝试架构固件，再用固定槽传递
/// `asid + start + page_count`；生产 MM 同时传入 `mm_generation`，软件 handler
/// 会在失效后、ack 前发布本核 observed generation，避免返回用户态时重复全刷。
/// 槽被同 CPU 的意外重入占用时才保守退回全用户/non-global IPI。`range=None`
/// 始终执行全用户失效。调用方必须先释放
/// VM/PTE 及其它普通锁，并把撤映射 frame 保留到本函数成功返回。不同 MM 可以并发调用：
/// 精确请求由每 CPU 槽隔离，全量 fallback 则可安全合并到较新的 sequence。
pub(crate) fn synchronize_user_tlb(
    targets: usize,
    asid: u16,
    range: Option<crate::mm::VPNRange>,
    mm_generation: Option<(&crate::mm::TlbContext, usize)>,
) -> Result<(), UserTlbSyncError> {
    assert!(
        mm_generation.is_none() || range.is_some(),
        "MM generation is only meaningful for a precise user TLB range"
    );
    let range_pages = match range {
        Some(range) => {
            let start_vpn = range.get_start().0;
            let end_vpn = range.get_end().0;
            let pages = end_vpn.saturating_sub(start_vpn);
            if pages == 0 || pages > MAX_USER_TLB_RANGE_PAGES {
                return Err(UserTlbSyncError::InvalidRange {
                    start_vpn,
                    end_vpn,
                });
            }
            Some(pages)
        }
        None => None,
    };
    let configured = expected_online_mask();
    if targets == 0 || targets & !configured != 0 {
        return Err(UserTlbSyncError::InvalidTargets { targets });
    }
    let online = online_cpu_mask();
    if targets & !online != 0 {
        return Err(UserTlbSyncError::UnavailableTargets {
            targets,
            available: online & !stopped_cpu_mask(),
        });
    }

    // STOP 是不可恢复终态；已 stopped 的 CPU 不会再次使用旧用户翻译，所以其
    // 停止确认可以替代本轮 TLB ack。MangoCore 尚不支持 CPU hotplug。
    let live_targets = targets & !stopped_cpu_mask();
    if live_targets == 0 {
        return Ok(());
    }
    let current_bit = 1usize << self::cpu_id();
    let remote = live_targets & !current_bit;

    if remote == 0 {
        match range {
            Some(range) => crate::hal::user_tlb_invalidate_range(asid, range),
            None => crate::hal::user_tlb_invalidate(),
        }
        return Ok(());
    }
    let started_at = crate::hal::get_time();

    // RFENCE 直接接受硬件 hart mask，且调用返回就代表目标已完成失效；它没有
    // 可被并发发起者覆盖的共享 payload。LA64 返回 false，继续走固定区间槽。
    if let Some(range) = range {
        let pages = range_pages.expect("validated user TLB range lost its page count");
        match crate::hal::remote_user_tlb_invalidate_range(live_targets, asid, range) {
            Ok(true) => {
                record_tlb_shootdown(
                    TlbShootdownKind::UserRangeFirmware { pages },
                    remote,
                    started_at,
                    false,
                );
                return Ok(());
            }
            Ok(false) => {}
            Err(error) => {
                record_tlb_shootdown(
                    TlbShootdownKind::UserRangeFirmware { pages },
                    remote,
                    started_at,
                    true,
                );
                return Err(UserTlbSyncError::Firmware { error });
            }
        }

        // 每个 CPU 只会同步等待一轮 shootdown，因此优先使用自己的固定槽。
        // CAS 失败说明出现了重入或上一轮 fail-stop 残留；全刷比覆盖 payload 安全。
        let slot = &USER_TLB_RANGE_SLOTS[self::cpu_id()];
        if slot.try_claim() {
            slot.publish(remote, asid, range, mm_generation);
            if live_targets & current_bit != 0 {
                crate::hal::user_tlb_invalidate_range(asid, range);
                if let Some((context, generation)) = mm_generation {
                    context.mark_cpu_observed(generation, self::cpu_id());
                }
            }
            let send_error = send_ipi_mask(remote, IpiReason::USER_TLB_RANGE_SYNC).err();
            let _irq_guard = IpiWaitIrqGuard::enter();
            let deadline = crate::hal::get_time().saturating_add(
                crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS),
            );
            loop {
                let acknowledged = slot.acknowledged();
                let missing = remote & !stopped_cpu_mask() & !acknowledged;
                if missing == 0 {
                    slot.release();
                    record_tlb_shootdown(
                        TlbShootdownKind::UserRangeIpi { pages },
                        remote,
                        started_at,
                        false,
                    );
                    return Ok(());
                }
                if crate::hal::get_time() >= deadline {
                    // 不释放槽：迟到的目标只能看到本轮原 payload，不能把 stale
                    // doorbell 错配到后续请求。正常 TlbFlush 会在返回错误后 fail-stop。
                    record_tlb_shootdown(
                        TlbShootdownKind::UserRangeIpi { pages },
                        remote,
                        started_at,
                        true,
                    );
                    return Err(UserTlbSyncError::RangeTimeout {
                        missing,
                        acknowledged,
                        send_error,
                    });
                }
                spin_loop();
            }
        }
    }

    let mut expected = [0usize; MAX_CPUS];
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if remote & (1usize << cpu_id) == 0 {
            continue;
        }
        expected[cpu_id] = PER_CPUS[cpu_id]
            .user_tlb_request
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        assert_ne!(expected[cpu_id], 0, "user TLB sync sequence wrapped");
    }

    if live_targets & current_bit != 0 {
        crate::hal::user_tlb_invalidate();
    }
    let send_error = send_ipi_mask(remote, IpiReason::USER_TLB_SYNC).err();

    // 等待者本身也可能同时成为另一轮 user/kernel shootdown 的目标。临时开放
    // 本地中断后双方都能进入只做原子操作的 handler，不会形成 ack 环形等待。
    let _irq_guard = IpiWaitIrqGuard::enter();
    let deadline = crate::hal::get_time()
        .saturating_add(crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS));
    let result = loop {
        let stopped = stopped_cpu_mask();
        let mut missing = None;
        for cpu_id in 0..CONFIGURED_CPU_COUNT {
            if remote & (1usize << cpu_id) == 0 || stopped & (1usize << cpu_id) != 0 {
                continue;
            }
            let observed = PER_CPUS[cpu_id].user_tlb_ack.load(Ordering::Acquire);
            if observed < expected[cpu_id] {
                missing = Some((cpu_id, observed));
                break;
            }
        }
        let Some((cpu_id, observed)) = missing else {
            break Ok(());
        };
        if crate::hal::get_time() >= deadline {
            break Err(UserTlbSyncError::Timeout {
                cpu_id,
                expected: expected[cpu_id],
                observed,
                send_error,
            });
        }
        spin_loop();
    };
    let kind = if range.is_some() {
        TlbShootdownKind::UserRangeFallback
    } else {
        TlbShootdownKind::UserFull
    };
    record_tlb_shootdown(kind, remote, started_at, result.is_err());
    result
}

/// 在当前 CPU 的 timer IRQ fast path 发布一批待处理工作。
///
/// IRQ 只做原子记账；timer 队列、回调和调度都由后续安全点负责。Release
/// 与 `take_local_timer_pending()` 的 Acquire 配对，使安全点观察到本次
/// 中断在发布前完成的硬件静默操作。重复 IRQ 合并为一个 pending bit；
/// timer 使用绝对 deadline，因此不依赖中断次数推进语义。
pub fn publish_local_timer_interrupt() {
    let local = &PER_CPUS[self::cpu_id()];
    local.timer_irq_count.fetch_add(1, Ordering::Relaxed);
    local.timer_pending.store(true, Ordering::Release);
}

/// 为当前 CPU 建立第一个绝对调度 tick。
///
/// timer source 在本函数之后才会开放；CAS 把重复初始化直接变成启动错误，
/// 避免两套代码各自覆盖 deadline，制造难以复现的 tick 漂移。
pub(crate) fn init_local_sched_tick(deadline_ns: u64) {
    assert_ne!(deadline_ns, 0, "scheduler tick deadline cannot be zero");
    let local = &PER_CPUS[self::cpu_id()];
    assert!(
        local
            .sched_tick_deadline_ns
            .compare_exchange(0, deadline_ns, Ordering::Release, Ordering::Relaxed)
            .is_ok(),
        "CPU {} initialized its scheduler tick twice",
        self::cpu_id()
    );
}

/// 返回当前 CPU 下一次调度 tick 的绝对纳秒 deadline。
pub(crate) fn local_sched_tick_deadline() -> u64 {
    PER_CPUS[self::cpu_id()]
        .sched_tick_deadline_ns
        .load(Ordering::Acquire)
}

/// 若本地调度 tick 已到期，则按绝对时间推进到下一周期。
///
/// 中断可能被内核临界区推迟，因此不能简单执行 `deadline += period` 多次补账；
/// 落后一周期以上时直接从当前时间开始下一周期，避免安全点陷入追赶风暴。
pub(crate) fn advance_local_sched_tick(now_ns: u64, period_ns: u64) -> bool {
    let local = &PER_CPUS[self::cpu_id()];
    let deadline = local.sched_tick_deadline_ns.load(Ordering::Acquire);
    assert_ne!(deadline, 0, "local scheduler tick is not initialized");
    if now_ns < deadline {
        return false;
    }

    let next = deadline.saturating_add(period_ns);
    let next = if now_ns >= next {
        now_ns.saturating_add(period_ns)
    } else {
        next
    };
    local
        .sched_tick_deadline_ns
        .store(next, Ordering::Release);
    true
}

/// AP 通知 CPU0：全局 timer 队列出现了更早的 deadline。
///
/// 标志先于 doorbell 发布，因此 CPU0 即使正以 IRQ-off 状态运行 idle 循环，
/// 也能直接在下一轮安全点看到请求。IPI 发送失败不会丢 timer：CPU0 自己的
/// 周期 tick 最迟会在一个调度周期内重新扫描全局队列。
pub(crate) fn request_timer_reprogram() {
    let sender = self::cpu_id();
    assert_ne!(
        sender, BOOT_CPU_ID,
        "CPU0 must reprogram its timer without a self IPI"
    );
    PER_CPUS[BOOT_CPU_ID]
        .timer_reprogram_requested
        .store(true, Ordering::Release);
    let _ = send_ipi(BOOT_CPU_ID, IpiReason::TIMER_REPROGRAM);
}

/// CPU0 在关中断安全点消费一次可合并的 timer 重编程请求。
pub(crate) fn take_timer_reprogram_request() -> bool {
    if self::cpu_id() != BOOT_CPU_ID {
        return false;
    }
    PER_CPUS[BOOT_CPU_ID]
        .timer_reprogram_requested
        .swap(false, Ordering::Acquire)
}

/// 查询当前 CPU 是否仍有 deferred timer 工作。
pub fn local_timer_pending() -> bool {
    PER_CPUS[self::cpu_id()]
        .timer_pending
        .load(Ordering::Acquire)
}

/// 由当前 CPU 在关中断的安全点唯一消费 timer pending。
pub fn take_local_timer_pending() -> bool {
    PER_CPUS[self::cpu_id()]
        .timer_pending
        .swap(false, Ordering::Acquire)
}

/// 在 timer 队列、回调和精确重编程全部完成后记录一批 deferred 工作。
pub fn complete_local_timer_deferred() {
    PER_CPUS[self::cpu_id()]
        .timer_deferred_count
        .fetch_add(1, Ordering::Relaxed);
}

/// 查询一个 CPU 的 timer hard-IRQ 次数。
pub fn timer_irq_count(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].timer_irq_count.load(Ordering::Relaxed)
}

/// 查询一个 CPU 已完成的 deferred timer 批次数。
pub fn timer_deferred_count(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id]
        .timer_deferred_count
        .load(Ordering::Relaxed)
}

fn mark_cpu_idle(cpu_id: usize) {
    assert!(
        !PER_CPUS[cpu_id].idle.swap(true, Ordering::Release),
        "logical CPU {} entered its idle context more than once",
        cpu_id
    );
}

/// 由 CPU 自己唯一一次发布本地初始化完成。
fn mark_cpu_online(cpu_id: usize) {
    assert!(
        cpu_id < CONFIGURED_CPU_COUNT,
        "cannot publish unconfigured CPU {} online",
        cpu_id
    );

    // Release 与 online_cpu_mask() 的 Acquire 配对，发布本 CPU 在此前完成的
    // bootstrap 状态；CAS 同时把重复发布变成可诊断的不变量失败。
    assert!(
        PER_CPUS[cpu_id]
            .online
            .compare_exchange(false, true, Ordering::Release, Ordering::Relaxed)
            .is_ok(),
        "logical CPU {} published online more than once",
        cpu_id
    );
}

/// 由当前 CPU 在进入调度主循环前唯一一次发布 scheduler-entered ack。
pub(crate) fn mark_local_scheduler_entered() {
    let cpu_id = self::cpu_id();
    assert!(
        SCHEDULER_RELEASED.load(Ordering::Acquire),
        "CPU {} entered scheduler before BSP release",
        cpu_id
    );
    assert!(
        PER_CPUS[cpu_id]
            .scheduler_entered
            .compare_exchange(false, true, Ordering::Release, Ordering::Relaxed)
            .is_ok(),
        "CPU {} entered scheduler more than once",
        cpu_id
    );
}

/// 返回 BSP 是否已经发布调度器所需的全部全局初始化。
pub(crate) fn schedulers_released() -> bool {
    SCHEDULER_RELEASED.load(Ordering::Acquire)
}

/// 发布 scheduler-ready，并等待所有 AP 进入各自的本地调度循环。
///
/// 该函数必须在 VFS、任务 registry 等 kernel-only 任务依赖的全局对象完成
/// 初始化后调用。普通用户任务仍默认首次发布到 CPU0；B29 的受控迁移不能据此
/// 外推为已经解除用户 MM 与共享子系统限制。
pub fn release_secondary_schedulers() {
    assert_eq!(self::cpu_id(), BOOT_CPU_ID);
    let online = online_cpu_mask();
    let expected = expected_online_mask();
    assert_eq!(
        online, expected,
        "scheduler release requires every configured CPU online"
    );
    assert!(
        SCHEDULER_RELEASED
            .compare_exchange(false, true, Ordering::Release, Ordering::Relaxed)
            .is_ok(),
        "secondary schedulers released more than once"
    );

    let targets = expected & !(1usize << BOOT_CPU_ID);
    if targets == 0 {
        return;
    }
    request_reschedule_mask(targets).unwrap_or_else(|error| {
        panic!("failed to release secondary schedulers: error {}", error)
    });

    let deadline = crate::hal::get_time().saturating_add(
        crate::hal::get_clock_freq().saturating_mul(ONLINE_TIMEOUT_SECONDS),
    );
    loop {
        let entered = scheduler_cpu_mask();
        let missing = targets & !entered;
        if missing == 0 {
            return;
        }
        if crate::hal::get_time() >= deadline {
            panic!(
                "secondary scheduler timeout: targets={:#x} entered={:#x} missing={:#x}",
                targets, entered, missing
            );
        }
        spin_loop();
    }
}

/// 批量通知已经获得 runnable 任务的 CPU。
///
/// 调用者必须先释放 runqueue/TASK_MANAGER 锁，再调用本入口敲 doorbell。
pub(crate) fn request_reschedule_mask(targets: usize) -> Result<(), isize> {
    send_ipi_mask(targets, IpiReason::RESCHEDULE)
}

/// Convert the firmware/hardware ID at `_start` into MangoCore's logical ID.
///
/// OpenSBI may choose any configured hart for cold boot under MTTCG.  The
/// winner remains the physical BSP but is always exposed as logical CPU0.
pub fn register_cpu_entry(hardware_id: usize) -> usize {
    if hardware_id >= CONFIGURED_CPU_COUNT {
        crate::hal::boot_cpu_park();
    }

    // Only the cold-boot CPU can reach the kernel before CPU0 invokes the AP
    // start protocol, so it safely claims the sentinel.  Later APs read the
    // already-published hardware ID through the compare_exchange failure.
    let boot_hardware_id = match BOOT_HARDWARE_ID.compare_exchange(
        UNCLAIMED_BOOT_HARDWARE_ID,
        hardware_id,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => hardware_id,
        Err(existing) => existing,
    };
    let logical_id = hardware_to_logical_id(hardware_id, boot_hardware_id);
    install_cpu_local(logical_id);
    logical_id
}

/// Install and immediately verify this CPU's kernel-local pointer.
///
/// User mode temporarily owns the same GPR, so the user trap entry restores
/// this pointer before any Rust kernel code consumes it.
fn install_cpu_local(logical_id: usize) {
    let per_cpu = &PER_CPUS[logical_id];
    debug_assert_eq!(per_cpu.logical_id, logical_id);
    let expected = per_cpu as *const PerCpu as usize;
    crate::hal::install_cpu_local(expected);
    assert_eq!(
        crate::hal::cpu_local_ptr(),
        expected,
        "CPU-local register readback failed for logical CPU {}",
        logical_id
    );
}

/// Return the logical ID owned by the CPU executing this kernel code.
///
/// Validate the register as an array address before indexing `PER_CPUS`; a
/// corrupted trap offset must panic instead of becoming an arbitrary pointer
/// dereference in a later scheduler or IPI path.
pub fn cpu_id() -> usize {
    let ptr = crate::hal::cpu_local_ptr();
    let base = PER_CPUS.as_ptr() as usize;
    let stride = size_of::<PerCpu>();
    let offset = ptr.checked_sub(base).unwrap_or_else(|| {
        panic!(
            "CPU-local pointer {:#x} precedes PerCpu table {:#x}",
            ptr, base
        )
    });

    assert_eq!(
        offset % stride,
        0,
        "CPU-local pointer {:#x} is not PerCpu-aligned",
        ptr
    );
    let logical_id = offset / stride;
    assert!(
        logical_id < CONFIGURED_CPU_COUNT,
        "CPU-local pointer {:#x} selects unconfigured CPU {}",
        ptr,
        logical_id
    );
    assert_eq!(PER_CPUS[logical_id].logical_id, logical_id);
    logical_id
}

/// 尝试根据 CPU-local 寄存器取得本核表项。
///
/// panic 可能发生在 `_start` 安装 CPU-local 指针之前，因此诊断路径不能
/// 直接调用会再次 panic 的 `cpu_id()`。这里只做纯地址校验，不解引用
/// 未经验证的寄存器值。
fn try_local_per_cpu() -> Option<&'static PerCpu> {
    let ptr = crate::hal::cpu_local_ptr();
    let base = PER_CPUS.as_ptr() as usize;
    let stride = size_of::<PerCpu>();
    let offset = ptr.checked_sub(base)?;
    if offset % stride != 0 {
        return None;
    }
    let logical_id = offset / stride;
    if logical_id >= CONFIGURED_CPU_COUNT {
        return None;
    }
    let per_cpu = &PER_CPUS[logical_id];
    (per_cpu.logical_id == logical_id).then_some(per_cpu)
}

/// 返回本 CPU 的任务调度状态。
pub(crate) fn local_task_state() -> &'static crate::task::processor::CpuTaskState {
    &try_local_per_cpu()
        .expect("CPU-local task state requested before CPU-local initialization")
        .task_state
}

/// 返回指定逻辑 CPU 的调度状态。
///
/// 远程入队和负载采样通过该入口定位唯一的 per-CPU runqueue；调用方必须
/// 保证一次只持有一个 runqueue 锁。
pub(crate) fn task_state(cpu_id: usize) -> &'static crate::task::processor::CpuTaskState {
    assert!(
        cpu_id < CONFIGURED_CPU_COUNT,
        "task state requested for unconfigured CPU {}",
        cpu_id
    );
    &PER_CPUS[cpu_id].task_state
}

/// 不阻塞、不 panic 地尝试返回本 CPU 的任务调度状态。
pub(crate) fn try_local_task_state() -> Option<&'static crate::task::processor::CpuTaskState> {
    try_local_per_cpu().map(|per_cpu| &per_cpu.task_state)
}

const fn hardware_to_logical_id(hardware_id: usize, boot_hardware_id: usize) -> usize {
    if hardware_id == boot_hardware_id {
        BOOT_CPU_ID
    } else if hardware_id < boot_hardware_id {
        hardware_id + 1
    } else {
        hardware_id
    }
}

const fn logical_to_hardware_id(logical_id: usize, boot_hardware_id: usize) -> usize {
    if logical_id == BOOT_CPU_ID {
        boot_hardware_id
    } else if logical_id <= boot_hardware_id {
        logical_id - 1
    } else {
        logical_id
    }
}

/// 把 MangoCore 逻辑 CPU 位图转换为固件使用的物理 hart 位图。
///
/// cold-boot hart 始终映射成逻辑 CPU0，因此不能把两个位图直接等同；RFENCE
/// 和 IPI 都必须经过与启动阶段相同的逆映射。
pub(crate) fn logical_to_hardware_mask(logical_mask: usize) -> usize {
    assert_eq!(
        logical_mask & !expected_online_mask(),
        0,
        "logical CPU mask contains an unconfigured CPU"
    );
    let boot_hardware_id = BOOT_HARDWARE_ID.load(Ordering::Acquire);
    assert_ne!(
        boot_hardware_id, UNCLAIMED_BOOT_HARDWARE_ID,
        "hardware mask requested before boot CPU registration"
    );

    let mut hardware_mask = 0usize;
    for logical_id in 0..CONFIGURED_CPU_COUNT {
        if logical_mask & (1usize << logical_id) != 0 {
            hardware_mask |= 1usize << logical_to_hardware_id(logical_id, boot_hardware_id);
        }
    }
    hardware_mask
}

/// Entry for every non-boot CPU.
///
/// Before the Acquire succeeds, an AP may use only its boot stack and
/// `.data.boot`; the heap, page tables, console, and ordinary globals still
/// belong exclusively to CPU0.
pub fn secondary_main(cpu_id: usize) -> ! {
    // A QEMU/kernel topology mismatch must not let an unconfigured CPU touch
    // shared state.  It owns a reserved stack, so it can safely park here.
    if cpu_id >= CONFIGURED_CPU_COUNT {
        crate::hal::boot_cpu_park();
    }

    // CPU0's Release publishes BSS clearing and the minimal MM/console setup.
    // Acquire is required: a Relaxed load could observe the phase without the
    // initialized memory that the phase promises.
    while BOOT_PHASE.load(Ordering::Acquire) != AP_RELEASED {
        spin_loop();
    }

    // This routine is CPU-local.  It must not allocate, print, enable the
    // normal timer interrupt, or enter the legacy scheduler.
    crate::hal::bootstrap_init(cpu_id);

    // 从这里开始抛弃 boot stack。架构 trampoline 会把 a0 保留为 cpu_id，
    // 并直接跳到 secondary_idle_main；该调用按设计永不返回。
    crate::hal::enter_secondary_idle(cpu_id, secondary_idle_main);
}

/// AP 切换到独立 idle stack 后的第一个 Rust 入口。
extern "C" fn secondary_idle_main(cpu_id: usize) -> ! {
    assert_eq!(
        self::cpu_id(),
        cpu_id,
        "idle stack entry lost the CPU-local identity"
    );

    // online 是 BSP 继续启动的承诺，所以必须等新栈已经生效、idle 状态已经
    // 发布后再置位。BSP 的 Acquire 读到 online 时即可依赖这些先行操作。
    mark_cpu_idle(cpu_id);
    mark_cpu_online(cpu_id);

    // BSP 完成 VFS/任务等全局初始化前，AP 继续维护 IPI/STOP 能力但不得
    // 访问调度器。Acquire 观察到 scheduler-ready 后直接进入共用调度入口。
    loop {
        let irq_was_enabled = crate::hal::local_irq_save();
        if SCHEDULER_RELEASED.load(Ordering::Acquire) {
            // 页表根寄存器属于 CPU-local 状态。AP 此前只访问恒等映射的
            // text/data/idle stack；调度高虚拟地址 kernel stack 前必须安装
            // BSP 已构造完成的内核页表，并由 activate 完成本地 TLB 刷新。
            crate::mm::activate_kernel_page_table();
            // 先建立未来 deadline，再开放本地 timer source。此后 AP 只处理
            // 自己的调度 tick，全局 timer callback 仍由 CPU0 独占。
            crate::task::timer_cpu_init();
            crate::task::run_tasks();
        }
        if !service_secondary_ipi_work() {
            crate::hal::secondary_cpu_wait();
        }
        crate::hal::local_irq_restore(irq_was_enabled);
    }
}

/// Start/release APs and wait until the configured topology is online.
///
/// Called by CPU0 only, after the existing global MM and machine initialization
/// has completed but before filesystems, networking, or tasks are exposed.
pub fn bring_up_secondary_cpus() {
    let expected = expected_online_mask();
    let boot_hardware_id = BOOT_HARDWARE_ID.load(Ordering::Acquire);
    assert_ne!(
        boot_hardware_id, UNCLAIMED_BOOT_HARDWARE_ID,
        "BSP hardware ID was not registered"
    );

    // CPU0 与 AP 走同一发布协议。若启动流程被错误地重复执行，CAS 会立即
    // 报告重复 online，而不是静默覆盖状态。
    mark_cpu_online(BOOT_CPU_ID);

    extern "C" {
        fn _start();
    }
    // HSM 的 `start_addr` 是在 SATP=bare 时取指的物理地址；`_start` 的 Rust
    // 符号则是高半区链接虚拟地址。入口汇编已在 BSP 单核阶段冻结 image_paddr，
    // 所以在发布 AP 前用同一镜像基址反算物理入口，不能把高半区地址交给固件。
    let secondary_entry = crate::hal::boot::kernel_linked_to_phys(_start as usize);

    for cpu_id in 1..CONFIGURED_CPU_COUNT {
        let hardware_id = logical_to_hardware_id(cpu_id, boot_hardware_id);
        // RV64 uses OpenSBI HSM. LA64 uses QEMU's mailbox-plus-IPI slave ROM.
        if let Err(error) = crate::hal::start_secondary_cpu(hardware_id, secondary_entry) {
            panic!(
                "failed to start logical CPU {} (hardware {}) through firmware: error {}",
                cpu_id, hardware_id, error
            );
        }
    }

    // Every AP that observes this Release may now access the initialized
    // memory promised above and execute only its CPU-local bootstrap routine.
    BOOT_PHASE.store(AP_RELEASED, Ordering::Release);

    let timeout_ticks = crate::hal::get_clock_freq().saturating_mul(ONLINE_TIMEOUT_SECONDS);
    let deadline = crate::hal::get_time().saturating_add(timeout_ticks);

    loop {
        // online_cpu_mask() 对每个表项执行 Acquire；读到对应 bit 即证明该
        // CPU 已在 Release 前完成本地初始化。
        let online = online_cpu_mask();
        if online & expected == expected {
            crate::println!(
                "[smp] minimal boot ready: configured={} boot_hw_id={} online_mask={:#x}",
                CONFIGURED_CPU_COUNT,
                boot_hardware_id,
                online
            );
            return;
        }
        if crate::hal::get_time() >= deadline {
            let missing = expected & !online;
            panic!(
                "secondary CPU online timeout: expected={:#x} online={:#x} missing={:#x}",
                expected, online, missing
            );
        }
        spin_loop();
    }
}
