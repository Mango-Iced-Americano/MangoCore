//! MangoCore bootargs — thin wrapper re-exporting pure logic from mango-kernel-core.
//!
//! The parsing logic lives in `mango-kernel-core` so it can be unit-tested
//! with `cargo test` on the host.

pub use mango_kernel_core::bootargs::{BootConfig, BootMode, Cmdline};

/// 取得内核命令行。
///
/// BSP 发布平台快照后优先使用 FDT `/chosen/bootargs`；固件没有提供时
/// 保留构建期 `MANGO_CMDLINE`，从而不改变现有 QEMU 测试配置合同。
pub fn get_cmdline() -> &'static str {
    if let Some(cmdline) = crate::hal::platform::platform_cmdline() {
        return cmdline;
    }
    compiled_cmdline()
}

/// 返回构建期命令行，不查询尚在构造中的平台快照。
///
/// `PlatformInfo` 自身需要在 FDT 未提供 `/chosen/bootargs` 时使用该值，
/// 因而不能在 `spin::Once::call_once` 闭包内绕回 [`get_cmdline`]。
pub(crate) fn compiled_cmdline() -> &'static str {
    option_env!("MANGO_CMDLINE").unwrap_or("mango.mode=normal")
}

/// Load boot configuration from the command line.
pub fn load() -> BootConfig {
    BootConfig::from_cmdline(get_cmdline())
}
