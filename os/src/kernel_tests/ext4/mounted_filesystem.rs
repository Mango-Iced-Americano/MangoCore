//! Tests that inspect an already-mounted lwext4 instance.

/// Check the mounted-instance path contract when the selected ktest fixture
/// provides an ext4 block device.  The normal zero-drive fixture announces its
/// intentional topology skip instead of reporting an unqualified pass.
pub(super) fn test_lw_path_isolation() -> Result<(), &'static str> {
    use crate::fs::ext4_lwext4::ext4fs::Ext4FileSystem;

    let Some(device) = crate::drivers::block::block_devices()[0].clone() else {
        crate::println!(
            "[KTEST SKIP] ext4::lw_path_isolation: zero-drive fixture has no block device"
        );
        return Ok(());
    };
    let fs = Ext4FileSystem::open_ext4rs(device)
        .map_err(|_| "ktest block device is not a mountable lwext4 filesystem")?;
    let root = fs.lw_path("/");
    if root.is_empty() {
        return Err("lw_path(\"/\") returned empty string");
    }
    let foo = fs.lw_path("/foo");
    let bar = fs.lw_path("/bar");
    if !foo.starts_with(&root) || foo == bar {
        return Err("lw_path did not preserve the mounted-instance path contract");
    }
    if fs.dev_id() == 0 {
        return Err("Ext4FileSystem dev_id should be non-zero");
    }
    Ok(())
}
