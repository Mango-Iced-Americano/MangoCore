//! BSP/AP 最小启动握手。
//!
//! Phase 1 在调度、中断和共享子系统启用前停驻 AP。每个 AP 只发布自己
//! `PerCpu` 表项中的 online 状态，不维护第二份全局 online 真相。

use core::{
    hint::spin_loop,
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const BOOT_CPU_ID: usize = 0;
pub const MAX_CPUS: usize = 8;

/// Phase 1 的 CPU-local 锚点；后续批次只扩展表项，不移动现有地址。
#[repr(C, align(64))]
struct PerCpu {
    logical_id: usize,
    online: AtomicBool,
    idle: AtomicBool,
}

impl PerCpu {
    const fn new(logical_id: usize) -> Self {
        Self {
            logical_id,
            online: AtomicBool::new(false),
            idle: AtomicBool::new(false),
        }
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
/// Phase 1 的 AP 一旦置位便永久停驻；Phase 2 会复用该字段实现 idle 与远程
/// 唤醒的握手，而不是再引入一份独立状态。
pub fn idle_cpu_mask() -> usize {
    let mut mask = 0usize;
    for cpu_id in 0..CONFIGURED_CPU_COUNT {
        if PER_CPUS[cpu_id].idle.load(Ordering::Acquire) {
            mask |= 1usize << cpu_id;
        }
    }
    mask
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

    // Phase 1 仍不允许 AP 进入旧调度器；Phase 2 会把永久 park 替换为可被
    // IPI 唤醒的 idle loop。
    crate::hal::boot_cpu_park();
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
