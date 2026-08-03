//! 固件进入内核时的原始启动信息。
//!
//! 启动汇编已经把硬件 CPU ID 与固件参数作为 `rust_main` 参数保留下来，
//! 因此这里不再增加一组可被多个 hart 同时覆盖的 `static mut RAW_*` 槽。
//! 逻辑 CPU0 在清 BSS 前发布一次快照，AP 只通过 [`boot_info`] 读取。

use spin::Once;

/// 固件把控制权交给内核时使用的协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "每个目标架构只构造自己的启动协议，Test 预留给独立 HAL 测试"
)]
pub enum BootProtocol {
    /// RISC-V SBI 标准入口：`a0=hartid`、`a1=dtb_paddr`。
    RiscvFdt,
    /// LoongArch direct boot/U-Boot：`a2` 指向 EFI system table。
    LoongArchEfi,
    /// 不依赖真实固件的内核测试环境。
    Test,
}

/// BSP 从入口参数冻结的只读启动快照。
#[derive(Debug, Clone, Copy)]
pub struct RawBootInfo {
    pub protocol: BootProtocol,
    /// 固件提供的硬件 CPU/hart ID；它不等同于 MangoCore 逻辑 CPU ID。
    pub hardware_id: usize,
    /// 固件协议的入口指针：RV64 为 FDT，LA64 为 EFI system table。
    pub firmware_arg_paddr: usize,
    /// 链接入口对应的物理地址，供后续可重定位启动使用。
    #[allow(dead_code, reason = "为后续可重定位 RV64 镜像迁移保留")]
    pub entry_paddr: usize,
    /// 内核镜像首字节的物理地址。
    #[allow(dead_code, reason = "为后续可重定位 RV64 镜像迁移保留")]
    pub image_paddr: usize,
}

/// `.data.boot` 不会被 BSP 的 BSS 清理覆盖；`spin::Once` 内部完成
/// Release 发布与 Acquire 读取，AP 不需要再依赖未证明的裸全局引用。
#[link_section = ".data.boot"]
static BOOT_INFO: Once<RawBootInfo> = Once::new();

/// 由逻辑 CPU0 在清 BSS 前保存固件入口参数。
pub fn init_bsp(hardware_id: usize, boot_arg: usize) {
    assert_eq!(
        crate::smp::cpu_id(),
        crate::smp::BOOT_CPU_ID,
        "only the BSP may publish boot info"
    );
    assert!(BOOT_INFO.get().is_none(), "boot info initialized twice");

    let protocol = detect_protocol();
    extern "C" {
        fn _start();
        fn skernel();
    }

    BOOT_INFO.call_once(|| RawBootInfo {
        protocol,
        hardware_id,
        firmware_arg_paddr: boot_arg,
        entry_paddr: _start as *const () as usize,
        image_paddr: skernel as *const () as usize,
    });
}

/// 取得 BSP 已发布的不可变启动快照。
pub fn boot_info() -> &'static RawBootInfo {
    BOOT_INFO.get().expect("boot info not initialized")
}

fn detect_protocol() -> BootProtocol {
    #[cfg(target_arch = "riscv64")]
    {
        BootProtocol::RiscvFdt
    }
    #[cfg(target_arch = "loongarch64")]
    {
        BootProtocol::LoongArchEfi
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    {
        BootProtocol::Test
    }
}
