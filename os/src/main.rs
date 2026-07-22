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
// Note: no crate-level allow(dead_code) — use #[expect(dead_code, reason = "...")]
// on specific items. Crate-level allows were removed in T10 rebaseline to
// prevent new dead code from being silently introduced.

#[cfg(all(feature = "initramfs", feature = "legacy_block_root"))]
compile_error!("features initramfs and legacy_block_root are mutually exclusive");
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
#[cfg(target_arch = "riscv64")]
#[path = "lang_items.rs.rv"]
mod lang_items;
#[cfg(target_arch = "loongarch64")]
#[path = "lang_items.rs.la"]
mod lang_items;
mod math;
mod mm;
mod net;
mod panic_diag;
mod random;
mod syscall;
mod task;
mod timer;
mod trace;
mod utils;

#[cfg(feature = "block_mem")]
use crate::config::DISK_IMAGE_BASE;
use crate::hal::bootstrap_init;
use crate::hal::machine_init;
// #[cfg(feature = "loongarch64")]
// core::arch::global_asm!(include_str!("hal/arch/loongarch64/entry.asm"));
#[cfg(feature = "riscv")]
core::arch::global_asm!(include_str!("hal/arch/riscv/entry.asm"));

// ── Initramfs root cpio (small boot root filesystem) ──
// The build script writes this only after validating the profile-selected CPIO.
#[cfg(all(feature = "initramfs", target_arch = "loongarch64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/initramfs.S")));
#[cfg(all(feature = "initramfs", target_arch = "riscv64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/initramfs.S")));

// ── Legacy: block_mem full rootfs image ──
#[cfg(all(feature = "block_mem", feature = "loongarch64"))]
core::arch::global_asm!(include_str!("load_img.S"));
#[cfg(all(feature = "block_mem", feature = "riscv"))]
core::arch::global_asm!(include_str!("load_img-rv.S"));

// ── Preload test payloads (initproc, bash, busybox, LTP) ──
// When preload_payloads feature is active AND we're not in block_mem mode
#[cfg(all(not(feature = "block_mem"), feature = "preload_payloads", feature = "riscv"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/preload_app.S")));
#[cfg(all(not(feature = "block_mem"), feature = "preload_payloads", feature = "loongarch64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/preload_app.S")));

// ── Legacy preload (no initramfs, no block_mem, no preload_payloads) ──
#[cfg(all(not(feature = "block_mem"), not(feature = "initramfs"), not(feature = "preload_payloads"), feature = "riscv"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/preload_app.S")));
#[cfg(all(not(feature = "block_mem"), not(feature = "initramfs"), not(feature = "preload_payloads"), feature = "loongarch64"))]
core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/preload_app.S")));

fn mem_clear() {
    extern "C" {
        fn sbss();
        fn ebss();
    }
    #[cfg(feature = "zero_init")]
    unsafe {
        core::slice::from_raw_parts_mut(
            sbss as usize as *mut u8,
            crate::config::MEMORY_END - sbss as usize,
        )
        .fill(0);
    }
    #[cfg(not(feature = "zero_init"))]
    unsafe {
        core::slice::from_raw_parts_mut(sbss as usize as *mut u8, ebss as usize - sbss as usize)
            .fill(0);
    }
}

#[cfg(feature = "block_mem")]
fn move_to_high_address() {
    extern "C" {
        fn simg();
        fn eimg();
    }
    unsafe {
        // 加载根文件系统镜像
        let img =
            core::slice::from_raw_parts(simg as usize as *mut u8, eimg as usize - simg as usize);
        // 以DISK_IMAGE_BASE到MEMORY_END上的内存作为根文件系统镜像
        let mem_disk = core::slice::from_raw_parts_mut(
            DISK_IMAGE_BASE as *mut u8,
            // 大小为256MB
            0x1000_0000,
        );
        // 清空mem_disk上的内容
        mem_disk.fill(0);
        mem_disk[..img.len()].copy_from_slice(img);
    }
}

#[no_mangle]
pub fn rust_main() -> ! {
    bootstrap_init();
    mem_clear();
    // 这一行可能有误，需要后续处理
    #[cfg(all(feature = "block_mem"))]
    move_to_high_address();
    console::log_init();
    trace::init();
    println!("[kernel] Console initialized.");
    mm::init();
    println!("[kernel] Hello, world!");
    // note that remap_test is currently NOT supported by LA64, for the whole kernel space is RW!
    // #[cfg(feature = "riscv")]
    // mm::remap_test();

    machine_init();
    crate::task::timer_subsystem_init();
    random::init();
    println!("[kernel] PRNG initialized.");

    // 尽早加载 bootargs — Regression/Ktest 模式需要跳过某些 init 步骤
    let boot_config = crate::bootargs::load();

    // ── Initramfs 启动路径 ──
    #[cfg(feature = "initramfs")]
    {
        // 在 mm::init() 之后创建 VFS_ROOT: 创建 RamFS + 解包 cpio + 挂载 devfs/proc/tmp
        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();

        // Regression 模式：跳过网卡和块设备初始化（纯 initramfs，无外部磁盘）
        if boot_config.mode != crate::bootargs::BootMode::Regression {
            drivers::init_net_device();
            net::config::init();

            // 先探测块设备（需要连续物理页 DMA，必须在 preload 分配页之前做）
            fs::mount_boot_block_devices();

            // 安装预装载的测试 payload（迁移期保留，在块设备探测之后避免页碎片化）
            #[cfg(feature = "preload_payloads")]
            fs::install_preload_payloads();
        } else {
            crate::println!("[kernel] Regression mode — skipping net/block init");
        }
    }

    // ── Legacy 启动路径（initramfs 特性未启用时）──
    #[cfg(not(feature = "initramfs"))]
    {
        drivers::init_net_device();
        net::config::init();
        #[cfg(feature = "block_virt")]
        println!("[kernel] block in virt mode!");
        #[cfg(feature = "oom_handler")]
        println!("[kernel] oom_handler is enabled!");
        #[cfg(feature = "heap_trace")]
        println!("[kernel] heap_trace is enabled!");
        fs::flush_preload();
        fs::mount_tools_disk();
    }

    crate::fs::vfs::posix_lock::init_posix_lock_manager();

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
