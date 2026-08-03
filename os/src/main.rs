#![no_std]
#![no_main]
#![feature(linkage)]
#![feature(asm_const)]
#![feature(naked_functions)]
#![feature(asm_experimental_arch)]
#![feature(panic_info_message)]
#![feature(alloc_error_handler)]
#![feature(int_roundings)]
#![feature(string_remove_matches)]
#![allow(internal_features)]
#![feature(lang_items)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::test_runner)]
#![feature(const_maybe_uninit_assume_init)]
#![feature(trait_upcasting)]
#![feature(core_intrinsics)]
#![deny(
    future_incompatible,
    improper_ctypes,
    clashing_extern_declarations,
    unexpected_cfgs,
    static_mut_refs
)]
// Note: no crate-level allow(dead_code) — use #[expect(dead_code, reason = "...")]
// on specific items. Crate-level allows were removed in T10 rebaseline to
// prevent new dead code from being silently introduced.

pub use hal::config;
extern crate alloc;
extern crate core;

#[macro_use]
extern crate bitflags;

#[macro_use]
mod console;
mod bootargs;
mod drivers;
mod fs;
mod hal;
mod kernel_tests;
mod lang_items;
mod math;
mod mm;
mod net;
mod panic_diag;
mod random;
mod smp;
mod syscall;
mod task;
mod timer;
mod trace;
mod utils;

use crate::hal::bootstrap_init;
use crate::hal::machine_init;
#[cfg(all(feature = "loongarch64", feature = "board_2k1000"))]
core::arch::global_asm!(include_str!("hal/arch/loongarch64/entry.asm"));
#[cfg(feature = "riscv")]
core::arch::global_asm!(include_str!("hal/arch/riscv/entry.asm"));

// ── Initramfs root cpio (small boot root filesystem) ──
// The build script writes this only after validating the profile-selected CPIO.
#[cfg(all(feature = "initramfs", target_arch = "loongarch64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/initramfs.S")));
#[cfg(all(feature = "initramfs", target_arch = "riscv64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/initramfs.S")));

fn mem_clear() {
    extern "C" {
        #[cfg(feature = "zero_init")]
        fn skernel();
        #[cfg(feature = "zero_init")]
        fn ekernel();
        fn sbss();
        fn ebss();
    }
    unsafe {
        core::slice::from_raw_parts_mut(sbss as usize as *mut u8, ebss as usize - sbss as usize)
            .fill(0);
    }

    #[cfg(feature = "zero_init")]
    {
        // `zero_init` 的 fresh-frame 快路径要求所有未来可分配页已经清零。不能再
        // 按编译期 MEMORY_END 清一段连续地址：LA64 有内存洞，QEMU 的 `-m`
        // 也会改变 RAM 末端。复用固件 region 迭代器，并排除整个内核镜像，既
        // 保护 boot stack/FDT 快照，也与随后 frame allocator 的所有权边界一致。
        let kernel_image = [(skernel as usize, ekernel as usize)];
        hal::firmware::for_each_usable_ram_range(&kernel_image, |start, end| unsafe {
            core::slice::from_raw_parts_mut(start as *mut u8, end - start).fill(0);
        });
    }
}

/// 双架构共用的固件入口。
///
/// RV64 由 OpenSBI 在 `a0/a1` 传入 hart ID 与 FDT；LA64 入口将 CPUID 和
/// 固件 `a2` 中的 EFI system table 整理为相同 ABI。这里必须先登记逻辑 CPU，
/// 随后才能决定当前 CPU 是否拥有全局初始化权。
#[no_mangle]
pub extern "C" fn rust_main(cpu_id: usize, boot_arg: usize) -> ! {
    let logical_cpu_id = smp::register_cpu_entry(cpu_id);
    if logical_cpu_id == smp::BOOT_CPU_ID {
        bsp_main(logical_cpu_id, cpu_id, boot_arg)
    } else {
        smp::secondary_main(logical_cpu_id)
    }
}

/// 只有逻辑 CPU0 可以进入原有的 MangoCore 全局初始化路径。
fn bsp_main(cpu_id: usize, hardware_id: usize, boot_arg: usize) -> ! {
    task::perf::record_boot_stage(task::perf::BOOT_STAGE_ENTRY);
    // 入口参数必须在任何架构初始化之前冻结，避免固件寄存器语义丢失；
    // AP 不得重复覆盖这份 `.data.boot` 快照。
    hal::boot::init_bsp(hardware_id, boot_arg);
    // Phase 1 中 AP 只执行 CPU-local 初始化并 park，因此 BSS、堆、驱动、
    // 文件系统和旧全局调度器仍由 CPU0 单独拥有。
    bootstrap_init(cpu_id);
    // LA64 必须先建立 DMW、异常入口和页表寄存器基线；RV64 的 FDT 又必须
    // 在清 BSS 前复制，所以固件资源发现固定在这两个启动边界之间。
    hal::firmware::discover_early_resources();
    mem_clear();
    console::log_init();
    trace::init();
    println!("[kernel] Console initialized.");
    mm::init();
    // PlatformInfo 内含 String/Vec，只能在堆可用后构造；bring_up AP 之前
    // 完成 Once 发布，保证 AP 后续只能看到完整的不可变对象。
    hal::platform::init_platform();
    let boot_info = hal::boot::boot_info();
    println!(
        "[kernel] Boot protocol: {:?}, hardware_id={}, firmware_arg={:#x}",
        boot_info.protocol, boot_info.hardware_id, boot_info.firmware_arg_paddr
    );
    let platform_info = hal::platform::platform_info();
    println!(
        "[kernel] Firmware resources: ram_regions={}, reserved={}, early_mmio={}, usable={} MiB",
        hal::firmware::memory_regions().len(),
        hal::firmware::firmware_reserved_regions().len(),
        hal::firmware::early_mmio_ranges().len(),
        hal::firmware::usable_memory_size() / (1024 * 1024),
    );
    println!(
        "[kernel] Platform: firmware={:?}, model={}, devices={}",
        platform_info.firmware,
        platform_info.model.as_deref().unwrap_or("unspecified"),
        platform_info.devices.len(),
    );
    println!("[kernel] Hello, world!");
    // note that remap_test is currently NOT supported by LA64, for the whole kernel space is RW!
    // #[cfg(feature = "riscv")]
    // mm::remap_test();

    machine_init();
    crate::task::timer_cpu_init();
    match random::init() {
        Ok(()) => println!("[kernel] PRNG initialized."),
        Err(e) => println!("[kernel] PRNG init warning: {:?}", e),
    }
    // CPU0 完成 BSS、内存、机器状态与全局随机源初始化后才 Release AP。
    // AP 只发布 online 并 park，后续 initramfs、驱动和 PID1 仍由 CPU0 执行。
    smp::bring_up_secondary_cpus();

    // 尽早加载 bootargs — Regression/Ktest 模式需要跳过某些 init 步骤
    let boot_config = crate::bootargs::load();

    // ── Initramfs 启动路径 ──
    #[cfg(feature = "initramfs")]
    {
        // 在 mm::init() 之后创建 VFS_ROOT: 创建 RamFS + 解包 cpio + 挂载 devfs bootstrap
        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();

        // Regression 模式：跳过网卡和块设备初始化（纯 initramfs，无外部磁盘）
        if boot_config.mode != crate::bootargs::BootMode::Regression {
            drivers::init_net_device();
            net::config::init();

            // 先探测块设备并注册 devfs 节点（需要连续物理页 DMA）。
            // PID1 owns the later x0/x1 mount policy.
            fs::register_boot_block_devices();
        } else {
            crate::println!("[kernel] Regression mode — skipping net/block init");
        }
    }

    crate::fs::vfs::posix_lock::init_posix_lock_manager();

    // 到这里，kernel-only 任务依赖的堆、VFS、task registry 和机器级状态
    // 已完成一次性初始化。Release 发布后 AP 才能进入本地调度循环；普通
    // 用户任务仍由下面的 CPU0 路径创建并固定在 CPU0。
    smp::release_secondary_schedulers();

    // ── Kernel self-test mode (mango.mode=ktest) ──
    // When ktest runs with the scheduler active, we spawn the test runner
    // as a kernel task and enter run_tasks().  The runner and any spawned
    // test helpers are the only tasks — initproc is *not* added.
    if boot_config.mode == crate::bootargs::BootMode::Ktest {
        crate::println!(
            "[kernel] Entering kernel test mode (ktest) — tests: {:?}, repeat: {}",
            boot_config.tests,
            boot_config.repeat,
        );
        // Store the config so the fn()-only trampoline can access it.
        *crate::kernel_tests::KTEST_BOOT_CONFIG.lock() = Some(boot_config);
        // Spawn the test runner as a kernel task.  It will run all
        // selected tests, then call hal::shutdown().
        // Spawned test helpers (wakers, additional waiters) run and exit
        // within the scheduler before the runner finishes.
        crate::task::spawn_ktest_task(crate::kernel_tests::run_ktest_entry);
        // Enter scheduler — ktest runner runs as a scheduled task.
        task::run_tasks();
    }

    // ── Normal boot ──
    task::add_initproc();
    // note that in run_tasks(), there is yet *another* pre_start_init(),
    // which is used to turn on interrupts in some archs like LoongArch.
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}

#[cfg(test)]
fn test_runner(_tests: &[&dyn Fn()]) {}
