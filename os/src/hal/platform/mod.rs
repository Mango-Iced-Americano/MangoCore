//! Platform abstraction layer.
//!
//! Defines the owned `PlatformInfo` model that the kernel consumes
//! after firmware handoff and the boot-profile defaults that remain
//! meaningful without a device description.

use core::cell::UnsafeCell;

pub mod info;
pub mod jh7110_cache;
#[allow(
    unused_imports,
    reason = "双架构共享公开类型面，部分类型只在 RV64 FDT 后端构造"
)]
pub use info::{
    ConsoleInfo, DeviceInfo, DeviceKind, DeviceStatus, FirmwareKind, MmioRange, PciHost,
    PlatformInfo, RawProperty, RawPropertyError, RawPropertyValidity, ResourceValidity,
};

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
/// Called once after `mm::init()`.
///
/// # Panics
///
/// Panics if called more than once.
pub fn init_platform_info() {
    let platform_info = match crate::hal::firmware::build_platform_info() {
        Some(platform_info) => platform_info,
        #[cfg(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw"))]
        None => PlatformInfo::from_static(),
        #[cfg(not(all(target_arch = "loongarch64", feature = "boot_la_uboot_dmw")))]
        None => panic!("this platform requires a retained valid FDT description"),
    };

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

/// SMP 分支旧入口：BSP 在 `mm::init()` 后构造并发布平台快照。
pub fn init_platform() {
    assert_eq!(crate::smp::cpu_id(), crate::smp::BOOT_CPU_ID);
    init_platform_info();
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

/// Returns whether firmware identified a physical StarFive VisionFive board.
///
/// The platform snapshot is initialized before any supported caller reaches
/// its exit path.
pub fn is_real_board() -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        platform_info()
            .model
            .as_deref()
            .is_some_and(is_visionfive_model)
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        false
    }
}

pub(crate) fn is_visionfive_model(model: &str) -> bool {
    let mut has_starfive = false;
    let mut has_visionfive = false;
    for part in model.split(|character: char| !character.is_ascii_alphanumeric()) {
        has_starfive |= part.eq_ignore_ascii_case("starfive");
        has_visionfive |= part.eq_ignore_ascii_case("visionfive");
    }
    has_starfive && has_visionfive
}

/// Default path to the user-space init binary when `/init` is absent.
pub const fn default_init_path() -> &'static str {
    "/initproc"
}
