//! MangoCore bootargs — thin wrapper re-exporting pure logic from mango-kernel-core.
//!
//! The parsing logic lives in `mango-kernel-core` so it can be unit-tested
//! with `cargo test` on the host.

pub use mango_kernel_core::bootargs::{BootConfig, BootMode, Cmdline};

/// Get the kernel command line string.
///
/// Precedence: DTB `/chosen/bootargs` > compile-time `MANGO_CMDLINE` >
/// built-in default.
pub fn get_cmdline() -> &'static str {
    if let Some(cmdline) = crate::hal::platform::platform_cmdline() {
        return cmdline;
    }
    option_env!("MANGO_CMDLINE").unwrap_or("mango.mode=normal")
}

/// Load boot configuration from the command line.
pub fn load() -> BootConfig {
    BootConfig::from_cmdline(get_cmdline())
}
