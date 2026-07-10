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
#![allow(dead_code)]
#![allow(unused_assignments)]
#![allow(unused_variables)]

#[cfg(all(feature = "initramfs", feature = "legacy_block_root"))]
compile_error!("features initramfs and legacy_block_root are mutually exclusive");
// 板级选择会改变链接基址、复位入口、UART 和地址位宽模型。必须在编译期拒绝歧义
// 组合，因为某些混合配置仍能成功链接，却会在 U-Boot 移交控制权后才发生故障。
#[cfg(all(feature = "board_laqemu", feature = "board_2k1000"))]
compile_error!("board_laqemu and board_2k1000 are mutually exclusive");
#[cfg(all(
    feature = "loongarch64",
    not(any(feature = "board_laqemu", feature = "board_2k1000"))
))]
compile_error!("loongarch64 requires exactly one board_laqemu or board_2k1000 feature");
#[cfg(all(feature = "board_2k1000", not(feature = "initramfs")))]
compile_error!("board_2k1000 bring-up requires the initramfs feature");
#[cfg(all(feature = "board_2k1000", feature = "block_mem"))]
compile_error!("board_2k1000 does not define a reserved block_mem region");
pub use hal::config;
extern crate alloc;
extern crate core;

#[macro_use]
extern crate bitflags;

#[macro_use]
mod console;
mod drivers;
mod fs;
mod hal;
mod lang_items;
mod math;
mod mm;
mod net;
mod panic_diag;
mod syscall;
mod task;
mod timer;
mod trace;
mod utils;

#[cfg(feature = "block_mem")]
use crate::config::DISK_IMAGE_BASE;
use crate::hal::bootstrap_init;
use crate::hal::machine_init;
// U-Boot 通过 DMW 缓存别名进入开发板内核。汇编入口会保留当前执行段、建立低地址
// 直映窗口、切换到链接时使用的 0x90000000 地址，并在进入 Rust 前创建启动栈。
// QEMU 在 loongarch64/boot.rs 中有独立的 Rust `_start`，同时编译两者会定义两个
// 不兼容的复位入口。
#[cfg(all(feature = "loongarch64", feature = "board_2k1000"))]
core::arch::global_asm!(include_str!("hal/arch/loongarch64/entry.asm"));
#[cfg(feature = "riscv")]
core::arch::global_asm!(include_str!("hal/arch/riscv/entry.asm"));

// ── Initramfs root cpio (small boot root filesystem) ──
#[cfg(all(feature = "initramfs", feature = "loongarch64"))]
core::arch::global_asm!(include_str!("initramfs-la.S"));
#[cfg(all(feature = "initramfs", feature = "riscv"))]
core::arch::global_asm!(include_str!("initramfs-rv.S"));

// ── Legacy: block_mem full rootfs image ──
#[cfg(all(feature = "block_mem", feature = "loongarch64"))]
core::arch::global_asm!(include_str!("load_img.S"));
#[cfg(all(feature = "block_mem", feature = "riscv"))]
core::arch::global_asm!(include_str!("load_img-rv.S"));

// ── Preload test payloads (initproc, bash, busybox, LTP) ──
// When preload_payloads feature is active AND we're not in block_mem mode
#[cfg(all(not(feature = "block_mem"), feature = "preload_payloads", feature = "riscv"))]
core::arch::global_asm!(include_str!("preload_app-rv.S"));
#[cfg(all(not(feature = "block_mem"), feature = "preload_payloads", feature = "loongarch64"))]
core::arch::global_asm!(include_str!("preload_app.S"));

// ── Legacy preload (no initramfs, no block_mem, no preload_payloads) ──
#[cfg(all(not(feature = "block_mem"), not(feature = "initramfs"), not(feature = "preload_payloads"), feature = "riscv"))]
core::arch::global_asm!(include_str!("preload_app-rv.S"));
#[cfg(all(not(feature = "block_mem"), not(feature = "initramfs"), not(feature = "preload_payloads"), feature = "loongarch64"))]
core::arch::global_asm!(include_str!("preload_app.S"));

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

    // ── Initramfs 启动路径 ──
    #[cfg(feature = "initramfs")]
    {
        // 在 mm::init() 之后创建 VFS_ROOT: 创建 RamFS + 解包 cpio + 挂载 devfs/proc/tmp
        #[cfg(feature = "board_2k1000")]
        {
            // 2K1000LA 实板首阶段只验证 U-Boot -> uImage -> UART -> initramfs。
            // 当前构建仍可能携带 `block_virt_pci` / `block_sata` feature；在 SATA/AHCI
            // 和板载网卡路径逐项验证前，先禁止任何外部块设备延迟探测，避免早期
            // 误扫 QEMU virtio PCI 或未稳定的 AHCI 路径导致串口首启被干扰。
            fs::force_ramfs();
            println!("[kernel] 2K1000 board bring-up: ramfs-only block path enabled");
        }

        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();

        #[cfg(not(feature = "board_2k1000"))]
        drivers::init_net_device();
        #[cfg(feature = "board_2k1000")]
        {
            // 实板网卡不是 QEMU virtio-net；最小上板阶段保留回环接口和网络核心，
            // 暂不枚举 virtio PCI 网卡，后续再接 GMAC/PHY 驱动。
            println!("[kernel] 2K1000 board bring-up: external net probe skipped");
        }
        net::config::init();

        // QEMU 路径先探测块设备（需要连续物理页 DMA，必须在 preload 分配页之前做）。
        // 2K1000 实板最小上板路径已在上方 force_ramfs()，此处不触发 BLOCK_DEVICES。
        #[cfg(not(feature = "board_2k1000"))]
        fs::mount_boot_block_devices();
        #[cfg(feature = "board_2k1000")]
        println!("[kernel] 2K1000 board bring-up: block device mount skipped");

        // 安装预装载的测试载荷。QEMU 路径在块设备探测之后执行，以减少 DMA 页碎片；
        // 2K1000 最小上板路径无外部块设备，直接安装到 initramfs/ramfs 根。
        #[cfg(feature = "preload_payloads")]
        {
            #[cfg(feature = "board_2k1000")]
            println!("[bringup][main:01] preload payload installation begin");
            fs::install_preload_payloads();
            #[cfg(feature = "board_2k1000")]
            println!("[bringup][main:02] preload payload installation complete");
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
    #[cfg(feature = "board_2k1000")]
    println!("[bringup][main:03] init task construction begin");
    task::add_initproc();
    #[cfg(feature = "board_2k1000")]
    println!("[bringup][main:04] init task queued; entering scheduler");
    // note that in run_tasks(), there is yet *another* pre_start_init(),
    // which is used to turn on interrupts in some archs like LoongArch.
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}

#[cfg(test)]
fn test_runner(_tests: &[&dyn Fn()]) {}
