//! 运行期平台描述。
//!
//! BSP 在堆初始化后构造唯一的 [`PlatformInfo`]；`spin::Once` 负责把完整
//! 对象发布给随后启动的 AP。设备、驱动和 ABI 查询只能读取该快照，不能
//! 在运行期回写固件解析结果。

use spin::Once;

mod info;
#[allow(
    unused_imports,
    reason = "双架构共享公开类型面，部分类型只在 RV64 FDT 后端构造"
)]
pub use info::{
    ConsoleInfo, DeviceInfo, DeviceKind, DeviceStatus, FirmwareKind, MmioRange, PciHost,
    PlatformInfo, RawProperty, RawPropertyError, RawPropertyValidity, ResourceValidity,
};

static PLATFORM_INFO: Once<PlatformInfo> = Once::new();

/// 由 BSP 在 `mm::init()` 后构造并发布平台快照。
pub fn init_platform() {
    assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    assert!(
        PLATFORM_INFO.get().is_none(),
        "platform info initialized twice"
    );

    PLATFORM_INFO.call_once(|| match crate::hal::firmware::build_platform_info() {
        Some(platform_info) => platform_info,
        #[cfg(all(target_arch = "loongarch64", feature = "board_2k1000"))]
        None => PlatformInfo::from_static(),
        #[cfg(not(all(target_arch = "loongarch64", feature = "board_2k1000")))]
        None => panic!("this platform requires a retained valid FDT description"),
    });
}

/// 取得已经发布的不可变平台快照。
pub fn platform_info() -> &'static PlatformInfo {
    PLATFORM_INFO.get().expect("platform info not initialized")
}

/// 在平台快照发布后返回固件命令行；初始化期间返回 `None` 以使用编译期值。
pub fn platform_cmdline() -> Option<&'static str> {
    PLATFORM_INFO.get().and_then(|platform_info| {
        (!platform_info.cmdline.is_empty()).then_some(platform_info.cmdline.as_str())
    })
}

#[expect(dead_code, reason = "后续 RV64 实板启动批次接入")]
pub fn is_real_board() -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        platform_info()
            .model
            .as_deref()
            .is_some_and(is_visionfive_model)
    }
    #[cfg(target_arch = "loongarch64")]
    {
        false
    }
}

#[expect(dead_code, reason = "由后续 RV64 实板启动批次接入")]
pub(crate) fn is_visionfive_model(model: &str) -> bool {
    let mut has_starfive = false;
    let mut has_visionfive = false;
    for part in model.split(|character: char| !character.is_ascii_alphanumeric()) {
        has_starfive |= part.eq_ignore_ascii_case("starfive");
        has_visionfive |= part.eq_ignore_ascii_case("visionfive");
    }
    has_starfive && has_visionfive
}
