//! Boot block device registry.
//!
//! Maps device names (e.g. "vda", "vdb", "mmcblk0") to block devices.
//! Future drivers (MMC, NVMe) register here via `register_boot_block()`.
//!
//! This module deliberately does not mount filesystems. The kernel discovers
//! hardware and exposes `/dev` nodes before PID1 owns mount policy.

use super::BlockDevice;
use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};
use alloc::{collections::BTreeMap, string::String, sync::Arc};
use spin::Mutex;

/// Global boot-block device registry.
/// Maps device names (e.g. "vda", "mmcblk0") to block devices.
static BOOT_BLOCK_REGISTRY: Mutex<Option<BTreeMap<String, Arc<dyn BlockDevice>>>> = Mutex::new(None);

/// Initialize the boot-block registry (called once during boot-block mount).
fn ensure_registry() -> &'static Mutex<Option<BTreeMap<String, Arc<dyn BlockDevice>>>> {
    let mut guard = BOOT_BLOCK_REGISTRY.lock();
    if guard.is_none() {
        *guard = Some(BTreeMap::new());
    }
    drop(guard);
    &BOOT_BLOCK_REGISTRY
}

/// Register a block device by name.
pub fn register_boot_block(name: &str, dev: Arc<dyn BlockDevice>) {
    let registry = ensure_registry();
    let mut guard = registry.lock();
    if let Some(map) = guard.as_mut() {
        map.insert(String::from(name), dev);
    }
}

/// Resolve a block device by its /dev name.
/// Returns None if the device is not registered.
pub fn resolve_block_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    let registry = ensure_registry();
    let guard = registry.lock();
    guard.as_ref()?.get(name).cloned()
}

/// Probe boot devices and register raw and partition devfs nodes.
pub(crate) fn register_boot_block_devices() {
    let devs = crate::drivers::block::block_devices();
    let sdcard = devs[0].clone();

    if let Some(blk0) = sdcard.as_ref() {
        let _ = crate::fs::dev::DEV_FS.add_dev(
            "vda",
            crate::fs::dev::block::BlockDevInode::new(blk0.clone(), 0, String::from("vda")),
        );
        register_boot_block("vda", blk0.clone());
    }

    match devs[1].as_ref() {
        None => {}
        Some(raw_vdb) => {
            let raw_vdb = raw_vdb.clone();
            let _ = crate::fs::dev::DEV_FS.add_dev(
                "vdb",
                crate::fs::dev::block::BlockDevInode::new(raw_vdb.clone(), 1, String::from("vdb")),
            );
            register_boot_block("vdb", raw_vdb.clone());

            match probe_mbr(&raw_vdb) {
                Ok(MbrProbe::NoMbr) => {}
                Ok(MbrProbe::Unsupported) => {}
                Ok(MbrProbe::Partitions(parts)) => {
                    for part in parts {
                        let part_dev = Arc::new(PartitionBlockDevice::new(
                            raw_vdb.clone(),
                            part.start_lba,
                            part.sectors,
                        )) as Arc<dyn BlockDevice>;
                        let name = alloc::format!("vdb{}", part.partno);
                        let _ = crate::fs::dev::DEV_FS.add_dev(
                            &name,
                            crate::fs::dev::block::BlockDevInode::new(
                                part_dev.clone(),
                                1 + part.partno as u64,
                                name.clone(),
                            ),
                        );
                        register_boot_block(&name, part_dev.clone());
                        println!(
                            "[mbr] registered /dev/{} (type={:#x}, size={}M)",
                            name,
                            part.type_code,
                            part.sectors * 512 / (1024 * 1024)
                        );

                        let alias = alloc::format!("vda{}", part.partno);
                        let _ = crate::fs::dev::DEV_FS.add_dev(
                            &alias,
                            crate::fs::dev::block::BlockDevInode::new(
                                part_dev.clone(),
                                100 + part.partno as u64,
                                alias.clone(),
                            ),
                        );
                        register_boot_block(&alias, part_dev.clone());
                    }
                }
                Err(_) => {}
            }
        }
    }
}
