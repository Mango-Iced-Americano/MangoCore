use alloc::sync::Arc;

use super::{vfs::FileSystem, BlockDevice};
use crate::utils::error::SyscallErr;

#[cfg(not(any(
    feature = "ext4_lwext4_backend",
    feature = "ext4_legacy_backend",
    feature = "ext4_another_backend",
)))]
compile_error!("exactly one ext4 backend feature must be selected");

#[cfg(any(
    all(feature = "ext4_lwext4_backend", feature = "ext4_legacy_backend"),
    all(feature = "ext4_lwext4_backend", feature = "ext4_another_backend"),
    all(feature = "ext4_legacy_backend", feature = "ext4_another_backend"),
))]
compile_error!("ext4 backend features are mutually exclusive");

pub fn open(block_device: Arc<dyn BlockDevice>) -> Result<Arc<dyn FileSystem>, SyscallErr> {
    #[cfg(all(
        feature = "ext4_lwext4_backend",
        not(any(feature = "ext4_legacy_backend", feature = "ext4_another_backend"))
    ))]
    {
        crate::println!("[ext4] backend: lwext4");
        let filesystem: Arc<dyn FileSystem> =
            super::ext4_lwext4::ext4fs::Ext4FileSystem::open_ext4rs(block_device)?;
        Ok(filesystem)
    }

    #[cfg(all(
        feature = "ext4_legacy_backend",
        not(any(feature = "ext4_lwext4_backend", feature = "ext4_another_backend"))
    ))]
    {
        crate::println!("[ext4] backend: legacy");
        let filesystem: Arc<dyn FileSystem> =
            super::ext4::ext4fs::Ext4FileSystem::open_ext4rs(block_device);
        Ok(filesystem)
    }

    #[cfg(all(
        feature = "ext4_another_backend",
        not(any(feature = "ext4_lwext4_backend", feature = "ext4_legacy_backend"))
    ))]
    {
        crate::println!("[ext4] backend: another_ext4");
        let filesystem: Arc<dyn FileSystem> =
            super::ext4_another::Ext4FileSystem::open(block_device)?;
        Ok(filesystem)
    }

    #[cfg(any(
        not(any(
            feature = "ext4_lwext4_backend",
            feature = "ext4_legacy_backend",
            feature = "ext4_another_backend"
        )),
        all(feature = "ext4_lwext4_backend", feature = "ext4_legacy_backend"),
        all(feature = "ext4_lwext4_backend", feature = "ext4_another_backend"),
        all(feature = "ext4_legacy_backend", feature = "ext4_another_backend")
    ))]
    {
        let _ = block_device;
        Err(SyscallErr::ENODEV)
    }
}
