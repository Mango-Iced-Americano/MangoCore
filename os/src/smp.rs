//! Minimal BSP/AP boot handshake.
//!
//! Phase 1 deliberately stops secondary CPUs before scheduling, interrupts, or
//! shared subsystems.  The three atomics below are the only state APs may touch.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicUsize, Ordering},
};

pub const BOOT_CPU_ID: usize = 0;
pub const MAX_CPUS: usize = 8;

/// Phase 1 CPU-local anchor; later batches extend this without moving entries.
#[repr(C, align(64))]
struct PerCpu {
    logical_id: usize,
}

impl PerCpu {
    const fn new(logical_id: usize) -> Self {
        Self { logical_id }
    }
}

// The immutable array lives outside BSS, so every early CPU may address its
// own cache-line-sized entry before CPU0 clears or initializes shared memory.
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

// build.rs rejects every value except the one-byte strings 1/2/4/8.
pub const CONFIGURED_CPU_COUNT: usize =
    (env!("MANGO_CORE_NUM").as_bytes()[0] - b'0') as usize;

const AP_RELEASED: usize = 1;
const ONLINE_TIMEOUT_SECONDS: usize = 5;
const UNCLAIMED_BOOT_HARDWARE_ID: usize = usize::MAX;

// These values must survive CPU0's BSS clear while LA64 APs are already
// polling them on their private boot stacks.
#[link_section = ".data.boot"]
static BOOT_HARDWARE_ID: AtomicUsize = AtomicUsize::new(UNCLAIMED_BOOT_HARDWARE_ID);
#[link_section = ".data.boot"]
static BOOT_PHASE: AtomicUsize = AtomicUsize::new(0);
#[link_section = ".data.boot"]
static ONLINE_MASK: AtomicUsize = AtomicUsize::new(0);

const fn expected_online_mask() -> usize {
    (1usize << CONFIGURED_CPU_COUNT) - 1
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
    install_boot_cpu_local(logical_id);
    logical_id
}

/// Install and immediately verify the boot-only CPU-local pointer.
///
/// No runtime caller may consume this pointer yet: user state can replace the
/// CPU-local GPR until trap entry learns to restore the kernel value.
fn install_boot_cpu_local(logical_id: usize) {
    let per_cpu = &PER_CPUS[logical_id];
    debug_assert_eq!(per_cpu.logical_id, logical_id);
    let expected = per_cpu as *const PerCpu as usize;
    crate::hal::install_boot_cpu_local(expected);
    assert_eq!(
        crate::hal::boot_cpu_local_ptr(),
        expected,
        "CPU-local register readback failed for logical CPU {logical_id}"
    );
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

    let cpu_bit = 1usize << cpu_id;
    // Release makes completion of local initialization precede publication of
    // the online bit.  Concurrent APs merge bits through one atomic RMW.
    ONLINE_MASK.fetch_or(cpu_bit, Ordering::Release);

    // Phase 1 APs remain outside every shared runtime path after becoming
    // online.  A later IPI batch will replace this permanent park loop.
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

    // CPU0 is already locally initialized.  This Relaxed store is sequenced
    // before BOOT_PHASE's Release, which performs the actual publication.
    ONLINE_MASK.store(1usize << BOOT_CPU_ID, Ordering::Relaxed);

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

    let timeout_ticks =
        crate::hal::get_clock_freq().saturating_mul(ONLINE_TIMEOUT_SECONDS);
    let deadline = crate::hal::get_time().saturating_add(timeout_ticks);

    loop {
        // Acquire pairs with each AP's Release fetch_or.  Seeing a bit therefore
        // means that CPU finished its local initialization before publication.
        let online = ONLINE_MASK.load(Ordering::Acquire);
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
