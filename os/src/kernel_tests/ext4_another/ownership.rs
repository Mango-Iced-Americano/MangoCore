use alloc::sync::Arc;

use crate::fs::vfs::FileSystem;

pub(super) fn test_root_inode_is_canonical_and_does_not_retain_filesystem(
) -> Result<(), &'static str> {
    let device = crate::drivers::block::block_device_by_role(
        crate::drivers::block::BlockDeviceRole::Root,
    )
    .ok_or("ktest requires a clean ext4 root block device")?;
    let fs = crate::fs::ext4_another::Ext4FileSystem::open(device)
        .map_err(|_| "another_ext4 root ownership fixture did not mount")?;
    let first_root = fs.root_inode();
    let second_root = fs.root_inode();
    if !Arc::ptr_eq(&first_root, &second_root) {
        return Err("root_inode did not return the canonical root inode object");
    }

    let filesystem = Arc::downgrade(&fs);
    drop(second_root);
    drop(first_root);
    drop(fs);
    if filesystem.upgrade().is_some() {
        return Err("canonical root inode retains its filesystem through a strong reference");
    }
    Ok(())
}
