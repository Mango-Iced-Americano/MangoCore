//! Boot block discovery and devfs registration.
//!
//! This module deliberately does not mount filesystems. The kernel discovers
//! hardware and exposes `/dev` nodes before PID1 owns mount policy.

use super::BlockDevice;
use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};
use alloc::{string::String, sync::Arc};

/// Probe boot devices and register raw and partition devfs nodes.
pub(crate) fn register_boot_block_devices() {
    let devs = crate::drivers::block::block_devices();
    let sdcard = devs[0].clone();

    if let Some(blk0) = sdcard.as_ref() {
        let _ = crate::fs::dev::DEV_FS.add_dev(
            "vda",
            crate::fs::dev::block::BlockDevInode::new(blk0.clone(), 0, String::from("vda")),
        );
    }

    match devs[1].as_ref() {
        None => {}
        Some(raw_vdb) => {
            let raw_vdb = raw_vdb.clone();
            let _ = crate::fs::dev::DEV_FS.add_dev(
                "vdb",
                crate::fs::dev::block::BlockDevInode::new(raw_vdb.clone(), 1, String::from("vdb")),
            );

            match probe_mbr(&raw_vdb) {
                MbrProbe::NoMbr => {}
                MbrProbe::Unsupported => {}
                MbrProbe::Partitions(parts) => {
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
                    }
                }
            }
        }
    }
}
