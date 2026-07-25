//! BSP/AP 最小启动握手与 IPI mailbox。
//!
//! Phase 1 建立 CPU-local 状态和独立 idle stack；Phase 2 首个子阶段只让
//! AP 响应无锁 IPI reason，仍不进入调度器或共享子系统。

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
    /// 本 CPU 是否已完成本地初始化；由所属 CPU Release 发布，其他 CPU Acquire 读取。
    online: AtomicBool,
    /// 本 CPU 是否已经切换到独立 idle stack；不表示此刻一定停在 idle 指令中。
    idle: AtomicBool,
    /// 尚未处理的 IPI 原因位图；发送方 Release 合并，目标 CPU Acquire 消费。
    pending_ipi: AtomicU32,
    /// 本 CPU 已处理的测试 PING 次数，供发送方确认 mailbox/doorbell/trap 闭环。
    ipi_ping_ack: AtomicUsize,
}

impl PerCpu {
    const fn new(logical_id: usize) -> Self {
        Self {
            logical_id,
            online: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            pending_ipi: AtomicU32::new(0),
            ipi_ping_ack: AtomicUsize::new(0),
        }
    }
}

// PING 只证明 mailbox、doorbell、trap 和 ack 闭环，不承载调度或 TLB 语义。
const IPI_PING: u32 = 1 << 0;

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
const UNCLAIMED_BOOT_HARDWARE_ID: usize = usize::MAX;

// These values must survive CPU0's BSS clear while LA64 APs are already
// polling them on their private boot stacks.
#[link_section = ".data.boot"]
static BOOT_HARDWARE_ID: AtomicUsize = AtomicUsize::new(UNCLAIMED_BOOT_HARDWARE_ID);
#[link_section = ".data.boot"]
static BOOT_PHASE: AtomicUsize = AtomicUsize::new(0);

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

/// 向一个已经 online 的逻辑 CPU 发布测试 PING，并返回应等待的 ack 序号。
///
/// `pending_ipi` 是按原因合并的 mailbox，不是事件队列；当前 Phase 2 子阶段
/// 只有 CPU0 串行发出下一次 PING，必须等前一次 ack 后才能再次使用同一 bit。
pub fn send_ipi_ping(cpu_id: usize) -> Result<usize, isize> {
    if cpu_id >= CONFIGURED_CPU_COUNT || cpu_id == self::cpu_id() {
        return Err(-3);
    }
    if !PER_CPUS[cpu_id].online.load(Ordering::Acquire) {
        return Err(-4);
    }

    let expected_ack = PER_CPUS[cpu_id]
        .ipi_ping_ack
        .load(Ordering::Relaxed)
        .wrapping_add(1);

    // Release 保证 mailbox reason 先于架构 doorbell 对目标 CPU 可见；接收端
    // 的 Acquire swap 与之配对。重复硬件中断只会看到空 mailbox，保持幂等。
    PER_CPUS[cpu_id]
        .pending_ipi
        .fetch_or(IPI_PING, Ordering::Release);
    let hardware_id = logical_to_hardware_id(
        cpu_id,
        BOOT_HARDWARE_ID.load(Ordering::Acquire),
    );
    crate::hal::send_ipi(hardware_id)?;
    Ok(expected_ack)
}

/// 查询目标 CPU 已处理的 PING 序号。
pub fn ipi_ping_ack(cpu_id: usize) -> usize {
    PER_CPUS[cpu_id].ipi_ping_ack.load(Ordering::Acquire)
}

/// 在当前 CPU 的硬中断上下文消费 mailbox。
///
/// handler 只做原子操作，不分配、不打印、不获取普通锁，也不直接调度。
pub fn handle_ipi() {
    let local = &PER_CPUS[self::cpu_id()];
    // Acquire 获取发送端在 Release fetch_or 前发布的数据；swap(0) 使原因只被
    // 当前 CPU 消费一次。doorbell 合并或重复到达都不会重复生成 ack。
    let reasons = local.pending_ipi.swap(0, Ordering::Acquire);
    if reasons & IPI_PING != 0 {
        // Release 把“本 CPU 已完成 handler”发布给等待方的 Acquire load。
        local.ipi_ping_ack.fetch_add(1, Ordering::Release);
    }
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

    // AP 仍不进入旧调度器，只在独立 idle stack 上响应 IPI-only 中断。
    crate::hal::secondary_cpu_idle();
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
