//! QEMU LoongArch64 virt platform policy.

use crate::hal::platform::PlatformPolicy;

pub struct QemuLaPolicy;

impl PlatformPolicy for QemuLaPolicy {
    fn name(&self) -> &'static str {
        "qemu-loongarch64"
    }
}
