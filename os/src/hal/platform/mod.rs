//! Platform abstraction layer.
//!
//! Defines the owned `PlatformInfo` model that the kernel consumes
//! after firmware handoff, plus the `PlatformPolicy` trait for
//! board-specific configuration.

use core::cell::UnsafeCell;

mod fallback;
pub mod info;
pub use info::{DeviceInfo, DeviceKind, FirmwareKind, PlatformInfo};

/// Global platform information singleton.
///
/// The boot path writes it exactly once before the scheduler starts; all later
/// access is read-only.
struct PlatformInfoCell(UnsafeCell<Option<PlatformInfo>>);

// SAFETY: `init_platform_info()` is called once during single-threaded boot
// before the scheduler starts. After initialization this cell is never
// mutated, so concurrent callers can only perform immutable reads.
unsafe impl Sync for PlatformInfoCell {}

/// Global platform information singleton.
/// Initialized once after `mm::init()`, read-only thereafter.
static PLATFORM_INFO: PlatformInfoCell = PlatformInfoCell(UnsafeCell::new(None));

/// Initialize the platform information singleton.
///
/// Called once after `mm::init()`. Tries FDT first, falls back to static.
///
/// # Panics
///
/// Panics if called more than once.
pub fn init_platform_info() {
    let boot_info = crate::hal::boot::boot_info();
    let platform_info = crate::hal::firmware::build_platform_info(boot_info.dtb_paddr)
        .unwrap_or_else(PlatformInfo::from_static);

    // SAFETY: [Categories 1 and 2 — aliasing and data races] This is the only
    // mutable access, performed during single-threaded boot before any reader
    // can run. The assertion prevents a second write; all later access is
    // immutable through `platform_info()` or `platform_cmdline()`.
    unsafe {
        let slot = &mut *PLATFORM_INFO.0.get();
        assert!(slot.is_none(), "platform info already initialized");
        *slot = Some(platform_info);
    }
}

/// Return a reference to the global `PlatformInfo`.
///
/// # Panics
///
/// Panics if called before [`init_platform_info`].
pub fn platform_info() -> &'static PlatformInfo {
    // SAFETY: [Category 1 — aliasing] Initialization completes before this
    // accessor is used, and the singleton is never mutated afterwards.
    unsafe {
        (*PLATFORM_INFO.0.get())
            .as_ref()
            .expect("platform info not initialized")
    }
}

/// Try to get the kernel command line from platform information.
///
/// Returns `None` before initialization or when firmware supplied an empty
/// command line.
pub fn platform_cmdline() -> Option<&'static str> {
    // SAFETY: [Category 1 — aliasing] The slot is read-only after its
    // single-threaded initialization. `PLATFORM_INFO` is static, so a borrow
    // of the contained `String` remains valid for the returned `'static` view.
    unsafe {
        (*PLATFORM_INFO.0.get()).as_ref().and_then(|platform_info| {
            (!platform_info.cmdline.is_empty()).then_some(platform_info.cmdline.as_str())
        })
    }
}

/// Platform-specific configuration provided by the compile-time selected policy.
///
/// Each platform (QEMU RV64, QEMU LA64, VisionFive 2, etc.) provides its own
/// implementation. The kernel boot sequence reads these values — it is NOT
/// delegated to the policy.
pub trait PlatformPolicy {
    /// Human-readable platform name.
    fn name(&self) -> &'static str;

    /// Default path to the user-space init binary.
    ///
    /// The kernel tries `/init` first, then falls back to this path.
    /// QEMU platforms default to `/initproc`; VF2 may use a board-specific init.
    fn init_path(&self) -> &'static str {
        "/initproc"
    }

    /// Default root device path (e.g. `/dev/vda`, `/dev/mmcblk0`).
    ///
    /// Used as the default when bootargs does not specify `root=` or `mango.root=`.
    fn default_root_device(&self) -> &'static str {
        "/dev/vda"
    }
}

/// Return the compile-time selected platform policy.
///
/// Feature flags determine which implementation is returned.
pub fn select_policy() -> &'static dyn PlatformPolicy {
    #[cfg(feature = "board_rvqemu")]
    {
        return &qemu_riscv::QemuRiscvPolicy;
    }
    #[cfg(feature = "board_vf2")]
    {
        return &vf2::VisionFive2Policy;
    }
    #[cfg(feature = "board_laqemu")]
    {
        return &qemu_la::QemuLaPolicy;
    }
    #[cfg(feature = "board_2k1000")]
    {
        return &qemu_la::QemuLaPolicy;
    }
    #[cfg(not(any(
        feature = "board_rvqemu",
        feature = "board_vf2",
        feature = "board_laqemu",
        feature = "board_2k1000",
    )))]
    {
        compile_error!("no platform board feature selected");
    }
}

// Sub-modules for each platform policy.
#[cfg(feature = "board_laqemu")]
mod qemu_la;
#[cfg(feature = "board_2k1000")]
mod qemu_la;
#[cfg(feature = "board_rvqemu")]
mod qemu_riscv;
#[cfg(feature = "board_vf2")]
mod vf2;
