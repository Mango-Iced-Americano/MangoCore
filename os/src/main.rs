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
mod syscall;
mod task;
mod timer;
mod trace;
mod utils;

use crate::hal::{bootstrap_init, machine_init};

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

#[no_mangle]
pub fn rust_main(hart_id: usize, dtb_paddr: usize) -> ! {
    crate::hal::boot::save_boot_info();
    bootstrap_init();
    // QEMU may place the DTB inside the kernel's large BSS image. Parse and
    // retain the pre-heap memory map before clearing that physical range.
    crate::hal::firmware::populate_memory_regions();
    mem_clear();
    console::log_init();
    trace::init();
    let _ = (hart_id, dtb_paddr);
    let bi = crate::hal::boot::boot_info();
    println!(
        "[kernel] Boot protocol: {:?}, hart_id={}, dtb_paddr={:#x}",
        bi.protocol, bi.hart_id, bi.dtb_paddr
    );
    println!("[kernel] Console initialized.");
    mm::init();
    println!("[kernel] Hello, world!");
    crate::hal::platform::init_platform_info();

    machine_init();
    crate::task::timer_subsystem_init();
    match random::init() {
        Ok(()) => println!("[kernel] PRNG initialized."),
        Err(e) => println!("[kernel] PRNG init warning: {:?}", e),
    }

    let policy = crate::hal::platform::select_policy();
    let mut boot_config = crate::bootargs::load();
    let cmdline = crate::bootargs::Cmdline::parse(crate::bootargs::get_cmdline());
    if cmdline.get("root").is_none() && cmdline.get("mango.root").is_none() {
        boot_config.root = policy.default_root_device().into();
    }

    #[cfg(feature = "initramfs")]
    {
        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();
        if boot_config.mode != crate::bootargs::BootMode::Regression {
            drivers::init_net_device();
            net::config::init();
            fs::mount_boot_block_devices(&boot_config);
        } else {
            crate::println!("[kernel] Regression mode — skipping net/block init");
        }
    }

    crate::fs::vfs::posix_lock::init_posix_lock_manager();

    if boot_config.mode == crate::bootargs::BootMode::Ktest {
        crate::println!(
            "[kernel] Entering kernel test mode (ktest) — tests: {:?}, repeat: {}",
            boot_config.tests, boot_config.repeat,
        );
        *crate::kernel_tests::KTEST_BOOT_CONFIG.lock() = Some(boot_config);
        crate::task::spawn_ktest_task(crate::kernel_tests::run_ktest_entry);
        task::run_tasks();
    }

    task::add_initproc();
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}

#[cfg(test)]
fn test_runner(_tests: &[&dyn Fn()]) {}
