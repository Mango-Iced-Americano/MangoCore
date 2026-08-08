use super::common::*;

pub fn sys_sync() -> isize {
    // Linux sync(2) returns success even when one writeback attempt fails;
    // the error remains observable through a later fsync/syncfs boundary.
    if let Err(error) = crate::fs::vfs::sync_all_backends() {
        log::error!("sys_sync: global filesystem sync failed: {:?}", error);
    }
    SUCCESS
}
