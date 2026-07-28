//! StarFive VisionFive 2 (JH7110) platform policy.

use crate::hal::platform::PlatformPolicy;

pub struct VisionFive2Policy;

impl PlatformPolicy for VisionFive2Policy {
    fn name(&self) -> &'static str {
        "visionfive2"
    }

    fn default_root_device(&self) -> &'static str {
        // VF2 typically boots from MMC.
        "/dev/mmcblk0"
    }
}
