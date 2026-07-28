//! QEMU RISC-V virt platform policy.

use crate::hal::platform::PlatformPolicy;

pub struct QemuRiscvPolicy;

impl PlatformPolicy for QemuRiscvPolicy {
    fn name(&self) -> &'static str {
        "qemu-riscv64"
    }
}
