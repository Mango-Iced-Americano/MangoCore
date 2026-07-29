//! BSP/AP 最小启动握手与 IPI mailbox。
//!
//! Phase 1 建立 CPU-local 状态和独立 idle stack；Phase 2 让 AP 响应无锁
//! IPI reason；Phase 3 在 BSP 发布 scheduler-ready 后让 AP 进入本地调度循环。

use core::{
    hint::spin_loop,
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
};

pub const BOOT_CPU_ID: usize = 0;
pub const MAX_CPUS: usize = 8;

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
    /// 目标 CPU 必须完成的 kernel-global 映射发布序号。
    kernel_tlb_request: AtomicUsize,
    /// 本 CPU 完成本地 TLB 刷新后发布的对应确认序号。
    kernel_tlb_ack: AtomicUsize,
    /// 目标 CPU 必须完成的全用户/non-global TLB 失效序号。
    user_tlb_request: AtomicUsize,
    /// 本 CPU 完成全用户 TLB 失效后发布的对应确认序号。
    user_tlb_ack: AtomicUsize,
    /// 尚未处理的 IPI 原因位图；发送方 Release 合并，目标 CPU Acquire 消费。
    pending_ipi: AtomicU32,
    /// 本 CPU 已处理的测试 PING 次数，供发送方确认 mailbox/doorbell/trap 闭环。
    ipi_ping_ack: AtomicUsize,
    /// 收到 round-trip 请求后，由 AP idle 上下文发送回复；IRQ handler 不发门铃。
    round_trip_reply_pending: AtomicBool,
    /// CPU0 已在本地 trap 中处理的 round-trip 回复序号。
    round_trip_reply_ack: AtomicUsize,
    /// 本 CPU 在 idle deferred 路径发送 IPI 失败的次数。
    ipi_send_failures: AtomicUsize,
    /// hard IRQ 已收到 STOP；真正停止必须延后到 AP 独立 idle stack。
    stop_requested: AtomicBool,
    /// 本 CPU 已承诺不再访问共享内核状态，供 CPU0 等待停机完成。
    stopped: AtomicBool,
    /// 本 CPU 是否有尚未在安全点处理的 timer 工作；多个 IRQ 可以合并。
    timer_pending: AtomicBool,
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
            kernel_tlb_request: AtomicUsize::new(0),
            kernel_tlb_ack: AtomicUsize::new(0),
            user_tlb_request: AtomicUsize::new(0),
            user_tlb_ack: AtomicUsize::new(0),
            pending_ipi: AtomicU32::new(0),
            ipi_ping_ack: AtomicUsize::new(0),
            round_trip_reply_pending: AtomicBool::new(false),
            round_trip_reply_ack: AtomicUsize::new(0),
            ipi_send_failures: AtomicUsize::new(0),
            stop_requested: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            timer_pending: AtomicBool::new(false),
            timer_irq_count: AtomicUsize::new(0),
            timer_deferred_count: AtomicUsize::new(0),
        }
    }
}

/// 可以合并进 per-CPU mailbox 的幂等 IPI 原因。
///
/// reason bit 只表示“至少处理一次”，不能表示事件次数；需要计数的协议必须
/// 另外使用 sequence/ack，并在复用同一 bit 前等待前一轮完成。
#[derive(Clone, Copy)]
pub struct IpiReason(u32);

impl IpiReason {
    /// 只用于证明 mailbox、doorbell、trap 和 ack 闭环。
    pub const PING: Self = Self(1 << 0);
    /// CPU0 请求 AP 在退出 hard IRQ 后回送一个真实 IPI。
    const ROUND_TRIP_REQUEST: Self = Self(1 << 1);
    /// AP idle 上下文向 CPU0 发布的 round-trip 回复。
    const ROUND_TRIP_REPLY: Self = Self(1 << 2);
    /// 请求 AP 在退出 hard IRQ 后停止，不再访问任何共享内核状态。
    const STOP: Self = Self(1 << 3);
    /// 目标 runqueue 已加入任务；handler 只发布 need-resched。
    const RESCHEDULE: Self = Self(1 << 4);
    /// BSP 已修改共享内核页表；目标必须刷新本地 TLB 后发布 ack。
    const KERNEL_TLB_SYNC: Self = Self(1 << 5);
    /// 某个用户 MM 的 PTE 已修改；目标必须清除本核全部用户翻译后 ack。
    const USER_TLB_SYNC: Self = Self(1 << 6);

    const fn bits(self) -> u32 {
        self.0
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
    Firmware { error: isize },
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

/// 向一组 online CPU 发布同一个幂等 reason，再逐个触发硬件 doorbell。
///
/// 所有 mailbox 都先完成 Release 发布，目标 CPU 才可能开始处理。若某个
/// doorbell 失败，已经发布的 reason 保留到后续 IPI 消费，不能回滚原子状态。
pub fn send_ipi_mask(targets: usize, reason: IpiReason) -> Result<(), isize> {
    let configured = expected_online_mask();
    if reason.bits() == 0 || targets & !configured != 0 {
        return Err(-3);
    }
    if targets & (1usize << self::cpu_id()) != 0 {
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

    let boot_hardware_id = BOOT_HARDWARE_ID.load(Ordering::Acquire);
    let mut first_error = None;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if targets & (1usize << cpu_id) != 0 {
            let hardware_id = logical_to_hardware_id(cpu_id, boot_hardware_id);
            // 一个 doorbell 失败不能阻止其余已发布 mailbox 的目标被唤醒；
            // 完成整轮发送后再返回首个错误，失败目标的 reason 留待后续 IPI。
            if let Err(error) = crate::hal::send_ipi(hardware_id) {
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

/// 发布测试 PING，并返回发送方应等待的 ack 序号。
///
/// 当前只有 CPU0 发送 PING；调用方必须等前一次 ack 后再对同一 CPU 调用，
/// 否则 bit 合并只承诺处理一次，不能生成两个 ack。
pub fn send_ipi_ping(cpu_id: usize) -> Result<usize, isize> {
    if cpu_id >= CONFIGURED_CPU_COUNT {
        return Err(-3);
    }
    let expected_ack = ipi_ping_ack(cpu_id).wrapping_add(1);
    send_ipi(cpu_id, IpiReason::PING)?;
    Ok(expected_ack)
}

/// 从 CPU0 发起一次 AP→BSP 硬件 IPI round-trip。
///
/// 请求 reason 仍由 AP hard IRQ 原子消费；AP 返回独立 idle 上下文后才发送
/// reply doorbell。调用方必须等待返回的 ack 后才能向同一目标复用 reason。
pub fn send_ipi_round_trip(cpu_id: usize) -> Result<usize, isize> {
    if self::cpu_id() != BOOT_CPU_ID || cpu_id == BOOT_CPU_ID || cpu_id >= CONFIGURED_CPU_COUNT {
        return Err(-3);
    }
    let expected = round_trip_reply_ack().wrapping_add(1);
    send_ipi(cpu_id, IpiReason::ROUND_TRIP_REQUEST)?;
    Ok(expected)
}

/// 查询目标 CPU 已处理的 PING 序号。
pub fn ipi_ping_ack(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].ipi_ping_ack.load(Ordering::Acquire)
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

/// 查询 CPU0 已处理的 round-trip 回复序号。
pub fn round_trip_reply_ack() -> usize {
    PER_CPUS[BOOT_CPU_ID]
        .round_trip_reply_ack
        .load(Ordering::Acquire)
}

/// 查询目标 CPU 在 deferred idle 路径发送 IPI 的失败次数。
pub fn ipi_send_failures(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id]
        .ipi_send_failures
        .load(Ordering::Acquire)
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
    // Acquire 获取发送端在 Release fetch_or 前发布的数据；swap(0) 使原因只被
    // 当前 CPU 消费一次。doorbell 合并或重复到达都不会重复生成 ack。
    let reasons = local.pending_ipi.swap(0, Ordering::Acquire);
    if reasons & IpiReason::PING.bits() != 0 {
        // Release 把“本 CPU 已完成 handler”发布给等待方的 Acquire load。
        local.ipi_ping_ack.fetch_add(1, Ordering::Release);
    }
    if reasons & IpiReason::ROUND_TRIP_REQUEST.bits() != 0 {
        // hard IRQ 只发布 deferred work；SBI/MMIO doorbell 由 idle 栈发送。
        local
            .round_trip_reply_pending
            .store(true, Ordering::Release);
    }
    if reasons & IpiReason::ROUND_TRIP_REPLY.bits() != 0 {
        local.round_trip_reply_ack.fetch_add(1, Ordering::Release);
    }
    if reasons & IpiReason::STOP.bits() != 0 {
        // 不可返回的 stop 不能发生在 trap frame 上；只向 idle 栈发布请求。
        local.stop_requested.store(true, Ordering::Release);
    }
    if reasons & IpiReason::RESCHEDULE.bits() != 0 {
        // runnable 已在发送方释放 runqueue 锁前完成发布；这里只留下无锁提示。
        local.need_resched.store(true, Ordering::Release);
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

    let mut did_work = local.need_resched.swap(false, Ordering::Acquire);
    if !local
        .round_trip_reply_pending
        .swap(false, Ordering::Acquire)
    {
        return did_work;
    }

    if send_ipi(BOOT_CPU_ID, IpiReason::ROUND_TRIP_REPLY).is_err() {
        local.ipi_send_failures.fetch_add(1, Ordering::Release);
    }
    did_work = true;
    did_work
}

/// 远程 runqueue 发布完成后唤醒目标 CPU。
pub(crate) fn request_reschedule(cpu_id: usize) -> Result<(), isize> {
    send_ipi(cpu_id, IpiReason::RESCHEDULE)
}

/// shootdown 等待期间临时开放本地中断，并在退出时恢复调用者原状态。
///
/// 当前发起者可能同时成为另一轮 shootdown 的目标；若双方都在 IRQ-off
/// 自旋，就会互相等待 ack。进入本 guard 前不得持有页表、runqueue 或普通锁。
/// 窗口内到达的 timer IRQ 仍只发布 deferred work；生产调用者随后必须经过
/// trap-return 或 scheduler timer 安全点，不能在 MM 同步层执行任意 timer callback。
struct TlbWaitIrqGuard {
    restore_enabled: bool,
}

impl TlbWaitIrqGuard {
    fn enter() -> Self {
        let restore_enabled = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(true);
        Self { restore_enabled }
    }
}

impl Drop for TlbWaitIrqGuard {
    fn drop(&mut self) {
        let _ = crate::hal::local_irq_save();
        crate::hal::local_irq_restore(self.restore_enabled);
    }
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
    let send_error = send_ipi_mask(remote, IpiReason::KERNEL_TLB_SYNC).err();

    let _irq_guard = TlbWaitIrqGuard::enter();
    let deadline = crate::hal::get_time()
        .saturating_add(crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS));
    loop {
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
                return Err(KernelTlbSyncError::UnavailableTargets {
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
            return Ok(());
        };
        if crate::hal::get_time() >= deadline {
            return Err(KernelTlbSyncError::Timeout {
                cpu_id,
                expected: expected[cpu_id],
                observed,
                send_error,
            });
        }
        spin_loop();
    }
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

/// 让一组曾缓存用户 MM 的 CPU 同步完成用户 TLB 失效。
///
/// `page=Some(vpn)` 时 RV64 优先使用 SBI RFENCE；固件不支持或 `page=None`
/// 时保守退回全用户/non-global IPI。调用方必须先释放
/// VM/PTE 及其它普通锁，并把撤映射 frame 保留到本函数成功返回。不同 MM 可以并发调用：
/// software fallback 每次覆盖本核全部用户项，因此 request 合并到较新序号仍覆盖旧请求。
pub(crate) fn synchronize_user_tlb(
    targets: usize,
    page: Option<crate::mm::VirtPageNum>,
) -> Result<(), UserTlbSyncError> {
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
        match page {
            Some(vpn) => crate::hal::user_tlb_invalidate_page(vpn),
            None => crate::hal::user_tlb_invalidate(),
        }
        return Ok(());
    }

    // RFENCE 直接接受硬件 hart mask，且调用返回就代表目标已完成失效；它没有
    // 可被并发发起者覆盖的共享 payload。LA64 返回 false，继续走下面的全量 IPI。
    if let Some(vpn) = page {
        match crate::hal::remote_user_tlb_invalidate_page(live_targets, vpn) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(UserTlbSyncError::Firmware { error }),
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
    let _irq_guard = TlbWaitIrqGuard::enter();
    let deadline = crate::hal::get_time()
        .saturating_add(crate::hal::get_clock_freq().saturating_mul(STOP_TIMEOUT_SECONDS));
    loop {
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
            return Ok(());
        };
        if crate::hal::get_time() >= deadline {
            return Err(UserTlbSyncError::Timeout {
                cpu_id,
                expected: expected[cpu_id],
                observed,
                send_error,
            });
        }
        spin_loop();
    }
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
/// 初始化后调用。普通用户任务仍固定在 CPU0，不能据此解除用户 MM 限制。
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
    let secondary_entry = _start as usize;

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
