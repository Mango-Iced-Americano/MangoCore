#![no_std]
#![no_main]
#![feature(linkage)]
#![feature(lint_reasons)]
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

#[cfg(all(
    target_arch = "loongarch64",
    not(any(feature = "boot_la_qemu", feature = "boot_la_uboot_dmw"))
))]
compile_error!("LA64 requires a LoongArch boot profile");
#[cfg(all(
    target_arch = "loongarch64",
    all(feature = "boot_la_qemu", feature = "boot_la_uboot_dmw")
))]
compile_error!("LoongArch boot profiles are mutually exclusive");

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

use crate::hal::{bootstrap_init, machine_init};

#[cfg(all(feature = "loongarch64", feature = "boot_la_uboot_dmw"))]
core::arch::global_asm!(include_str!("hal/arch/loongarch64/entry.asm"));
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(include_str!("hal/arch/riscv/image_header.S"));
#[cfg(target_arch = "riscv64")]
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
        let start = sbss as *const () as usize;
        let end = ebss as *const () as usize;
        core::slice::from_raw_parts_mut(start as *mut u8, end - start)
            .fill(0);
    }

    #[cfg(feature = "zero_init")]
    {
        // `zero_init` 的 fresh-frame 快路径要求所有未来可分配页已经清零。不能再
        // 按编译期 MEMORY_END 清一段连续地址：LA64 有内存洞，QEMU 的 `-m`
        // 也会改变 RAM 末端。复用固件 region 迭代器，并排除整个内核镜像，既
        // 保护 boot stack/FDT 快照，也与随后 frame allocator 的所有权边界一致。
        // 内核以 KERNEL_LINK_VADDR 链接，排除前必须先转成物理地址（与
        // `frame_allocator::for_each_usable_frame_region` 保持同一所有权边界）。
        let kernel_start = hal::boot::kernel_linked_to_phys(skernel as *const () as usize);
        let kernel_end = hal::boot::kernel_linked_to_phys(ekernel as *const () as usize);
        let kernel_image = [(kernel_start, kernel_end)];
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
    // 入口参数由 entry.asm 冻结到 RAW_* 槽，这里在任何架构初始化之前
    // 读取为不可变快照；AP 不得重复覆盖这份 `.data.boot` 数据。
    crate::hal::boot::save_boot_info();
    // Phase 1 中 AP 只执行 CPU-local 初始化并 park，因此 BSS、堆、驱动、
    // 文件系统和旧全局调度器仍由 CPU0 单独拥有。
    bootstrap_init(cpu_id);
    // LA64 必须先建立 DMW、异常入口和页表寄存器基线；RV64 的 FDT 又必须
    // 在清 BSS 前复制，所以固件资源发现固定在这两个启动边界之间。
    crate::hal::firmware::populate_memory_regions();
    mem_clear();
    console::log_init();
    trace::init();
    // bsp_main 的参数当前只用于 BSP/AP 分流；启动寄存器已由 entry.asm
    // 冻结进 RAW_* 槽并经 save_boot_info 快照，不再重复保存硬件参数。
    let _ = (hardware_id, boot_arg);
    let bi = crate::hal::boot::boot_info();
    println!(
        "[kernel] Boot protocol: {:?}, hart_id={}, dtb_paddr={:#x}",
        bi.protocol, bi.hart_id, bi.dtb_paddr
    );
    println!("[kernel] Console initialized.");
    mm::init();
    // PlatformInfo 内含 String/Vec，只能在堆可用后构造；bring_up AP 之前
    // 完成 Once 发布，保证 AP 后续只能看到完整的不可变对象。
    hal::platform::init_platform();
    crate::mm::init_zero_accelerator();
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
    // `init_platform()`（SMP 兼容入口）已在 mm::init() 后发布 PlatformInfo，
    // develop 侧 `init_platform_info()` 由它内部调用，无需再次初始化。
    #[cfg(target_arch = "riscv64")]
    crate::hal::configure_runtime_console();

    machine_init();
    crate::task::timer_cpu_init();
    match random::init() {
        Ok(()) => println!("[kernel] PRNG initialized."),
        Err(e) => println!("[kernel] PRNG init warning: {:?}", e),
    }
    // CPU0 完成 BSS、内存、机器状态与全局随机源初始化后才 Release AP。
    // AP 只发布 online 并 park，后续 initramfs、驱动和 PID1 仍由 CPU0 执行。
    smp::bring_up_secondary_cpus();

    let boot_config = crate::bootargs::load();

    #[cfg(feature = "initramfs")]
    {
        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();
        if boot_config.mode != crate::bootargs::BootMode::Regression {
            drivers::init_net_device();
            fs::mount_boot_block_devices(&boot_config);
        } else {
            crate::println!("[kernel] Regression mode — skipping block init");
        }
        // 网络始终初始化：零盘回归没有探测 NIC（NET_DEVICE 保持 None），
        // net::config::init() 退化为仅 loopback + null eth，为回归套件中的
        // loopback TCP/UDP 用例（net_tcp_accept/net_udp）提供 smoltcp 栈；
        // Unix socket、eventfd、epoll、futex 也依赖 NET_INTERFACE 结构就绪。
        net::config::init();
    }

    crate::fs::vfs::posix_lock::init_posix_lock_manager();

    // 到这里，kernel-only 任务依赖的堆、VFS、task registry 和机器级状态
    // 已完成一次性初始化。Release 发布后 AP 才能进入本地调度循环；PID1
    // 仍由下面的 CPU0 路径首次发布，随后在用户态扩展正式进程树的 affinity。
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
        *crate::kernel_tests::KTEST_BOOT_CONFIG.lock() = Some(boot_config);
        // ktest 也必须由任务上下文推进 smoltcp；hard IRQ 只发布 poll generation。
        // worker 和 runner 都固定在 CPU0，按 FIFO 顺序先让 worker建立等待协议。
        crate::task::spawn_kernel_worker("[net-poll]", crate::net::config::net_poll_worker);
        // Spawn the test runner as a kernel task.  It will run all
        // selected tests, then call hal::shutdown().
        // Spawned test helpers (wakers, additional waiters) run and exit
        // within the scheduler before the runner finishes.
        crate::task::spawn_ktest_task(crate::kernel_tests::run_ktest_entry);
        task::run_tasks();
    }

    task::add_initproc();
    // 先发布 PID1，再发布常驻 worker，确保 normal 启动仍由 PID1 首次获得
    // CPU0。worker 首次运行后进入 WaitQueue，不在 scheduler loop 内直接 poll。
    // 回归模式同样需要 worker：loopback TCP/UDP 用例会创建/关闭 socket，
    // 其 smoltcp 条目和 128 KiB 缓冲必须由 worker 的 socket-removal drain 回收，
    // 否则每个关闭的 UDP/TCP socket 都会在 kernel heap 上渐进泄漏。
    task::spawn_kernel_worker("[net-poll]", crate::net::config::net_poll_worker);
    // note that in run_tasks(), there is yet *another* pre_start_init(),
    // which is used to turn on interrupts in some archs like LoongArch.
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}

#[cfg(test)]
fn test_runner(_tests: &[&dyn Fn()]) {}
