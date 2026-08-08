use super::common::*;

pub fn sys_sync() -> isize {
    // Linux sync(2) returns success even when one writeback attempt fails;
    // the error remains observable through a later fsync/syncfs boundary.
    if let Err(error) = crate::fs::vfs::sync_all_backends() {
        log::error!("sys_sync: global filesystem sync failed: {:?}", error);
    }
    // develop 新增：同步生命周期 registry 之外直开的 another_ext4 实例。
    #[cfg(feature = "ext4_another_backend")]
    crate::fs::ext4_another::sync_all_instances();
    SUCCESS
}
