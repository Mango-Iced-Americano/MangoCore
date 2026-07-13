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
#[cfg(all(feature = "sata_probe", feature = "sata_write_probe"))]
compile_error!("sata_probe and sata_write_probe are mutually exclusive");
#[cfg(all(
    feature = "sata_fs_write_probe",
    any(feature = "sata_probe", feature = "sata_write_probe")
))]
compile_error!("sata_fs_write_probe cannot be combined with another SATA probe");
#[cfg(all(feature = "sata_scratch_rw", not(feature = "board_2k1000")))]
compile_error!("sata_scratch_rw is supported only on board_2k1000");
#[cfg(all(feature = "board_core_test", not(feature = "board_2k1000")))]
compile_error!("board_core_test is supported only on board_2k1000");
#[cfg(all(feature = "board_core_test", not(feature = "sata_scratch_rw")))]
compile_error!("board_core_test requires the writable SATA scratch workspace");
#[cfg(all(
    feature = "sata_scratch_rw",
    any(
        feature = "sata_probe",
        feature = "sata_write_probe",
        feature = "sata_fs_write_probe"
    )
))]
compile_error!("sata_scratch_rw cannot be combined with a SATA probe");
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
#[cfg(all(
    not(feature = "block_mem"),
    feature = "preload_payloads",
    feature = "riscv"
))]
core::arch::global_asm!(include_str!("preload_app-rv.S"));
#[cfg(all(
    not(feature = "block_mem"),
    feature = "preload_payloads",
    feature = "loongarch64"
))]
core::arch::global_asm!(include_str!("preload_app.S"));

// ── Legacy preload (no initramfs, no block_mem, no preload_payloads) ──
#[cfg(all(
    not(feature = "block_mem"),
    not(feature = "initramfs"),
    not(feature = "preload_payloads"),
    feature = "riscv"
))]
core::arch::global_asm!(include_str!("preload_app-rv.S"));
#[cfg(all(
    not(feature = "block_mem"),
    not(feature = "initramfs"),
    not(feature = "preload_payloads"),
    feature = "loongarch64"
))]
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
    boot_trace!("[kernel] Console initialized.");
    mm::init();
    boot_trace!("[kernel] Hello, world!");
    // note that remap_test is currently NOT supported by LA64, for the whole kernel space is RW!
    // #[cfg(feature = "riscv")]
    // mm::remap_test();

    machine_init();
    crate::task::timer_subsystem_init();

    if let Err(error) = random::init() {
        println!(
            "[kernel] random: secure source unavailable ({:?}); secure reads will fail",
            error
        );
    }

    // Non-destructive GMAC/PHY handoff inspection. This runs before any block
    // driver allocation and deliberately leaves U-Boot's MAC/PHY state intact.
    #[cfg(all(feature = "board_2k1000", feature = "gmac_probe"))]
    drivers::net::gmac_2k1000::probe_all();

    // Explicit opt-in validation for the integrated 2K1000 SATA controller.
    // This performs IDENTIFY and repeated reads of LBA0 only; force_ramfs below
    // remains active, so the SSD is neither mounted nor written.
    #[cfg(all(feature = "board_2k1000", feature = "sata_probe"))]
    drivers::block::sata_read_only_probe();

    // Explicit destructive validation. The probe verifies the prepared disk,
    // writes only beyond all MBR partitions, and restores the original sectors
    // before the ramfs-only boot continues.
    #[cfg(all(feature = "board_2k1000", feature = "sata_write_probe"))]
    drivers::block::sata_write_probe();

    // ── Initramfs 启动路径 ──
    #[cfg(feature = "initramfs")]
    {
        // 在 mm::init() 之后创建 VFS_ROOT: 创建 RamFS + 解包 cpio + 挂载 devfs/proc/tmp
        #[cfg(all(
            feature = "board_2k1000",
            any(
                not(feature = "block_sata"),
                feature = "sata_probe",
                feature = "sata_write_probe"
            )
        ))]
        {
            // 救援镜像和 sata_probe 镜像必须保持与文件系统探测解耦；普通的
            // board_2k1000 + block_sata 镜像才进入下方只读挂载路径。
            fs::force_ramfs();
            boot_trace!("[kernel] 2K1000 board bring-up: ramfs-only block path enabled");
        }
        #[cfg(all(
            feature = "board_2k1000",
            feature = "block_sata",
            not(any(feature = "sata_probe", feature = "sata_write_probe"))
        ))]
        boot_trace!("[kernel] 2K1000 board bring-up: SATA read-only mount enabled");

        crate::fs::vfs::posix_lock::init_posix_lock_manager();
        fs::initramfs_init();

        #[cfg(any(
            not(feature = "board_2k1000"),
            all(feature = "board_2k1000", feature = "gmac_2k1000")
        ))]
        drivers::init_net_device();
        #[cfg(all(feature = "board_2k1000", not(feature = "gmac_2k1000")))]
        {
            // 实板网卡不是 QEMU virtio-net；最小上板阶段保留回环接口和网络核心，
            // 暂不枚举 virtio PCI 网卡，后续再接 GMAC/PHY 驱动。
            boot_trace!("[kernel] 2K1000 board bring-up: external net probe skipped");
        }
        net::config::init();

        // 在安装 preload payload 前探测，保证 AHCI/virtio DMA 页仍可从低碎片
        // 物理内存中分配。
        #[cfg(not(feature = "board_2k1000"))]
        fs::mount_boot_block_devices();
        #[cfg(all(
            feature = "board_2k1000",
            feature = "block_sata",
            not(any(feature = "sata_probe", feature = "sata_write_probe"))
        ))]
        #[cfg(not(feature = "sata_scratch_rw"))]
        fs::mount_boot_block_devices_read_only();
        #[cfg(feature = "sata_scratch_rw")]
        fs::mount_boot_block_devices_with_writable_scratch();
        #[cfg(all(feature = "board_2k1000", feature = "sata_fs_write_probe"))]
        fs::run_board_scratch_write_probe();
        #[cfg(all(
            feature = "board_2k1000",
            any(
                not(feature = "block_sata"),
                feature = "sata_probe",
                feature = "sata_write_probe"
            )
        ))]
        boot_trace!("[kernel] 2K1000 board bring-up: block device mount skipped");

        // 安装预装载的测试载荷。QEMU 和 2K1000 SATA 路径都先完成块设备探测，
        // 以减少 DMA 页碎片；救援/probe 镜像则直接安装到 initramfs/ramfs 根。
        #[cfg(feature = "preload_payloads")]
        {
            #[cfg(feature = "board_2k1000")]
            boot_trace!("[bringup][main:01] preload payload installation begin");
            fs::install_preload_payloads();
            #[cfg(feature = "board_2k1000")]
            boot_trace!("[bringup][main:02] preload payload installation complete");
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
    boot_trace!("[bringup][main:03] init task construction begin");
    task::add_initproc();
    #[cfg(feature = "board_2k1000")]
    boot_trace!("[bringup][main:04] init task queued; entering scheduler");
    // note that in run_tasks(), there is yet *another* pre_start_init(),
    // which is used to turn on interrupts in some archs like LoongArch.
    task::run_tasks();
    panic!("Unreachable in rust_main!");
}

#[cfg(test)]
fn test_runner(_tests: &[&dyn Fn()]) {}
