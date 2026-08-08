use super::BlockDevice;
use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};
use crate::drivers::block::{BlockDeviceDescriptor, BlockDeviceNode, BlockDeviceNumber};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};
use lazy_static::*;
use spin::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootBlockRegistryError {
    DuplicateName,
    DuplicateDeviceNumber,
}

#[derive(Debug)]
pub enum BootBlockPublishError {
    Registry(BootBlockRegistryError),
    Devfs(crate::utils::error::SyscallErr),
}

struct RegisteredBlockDevice {
    device: Arc<dyn BlockDevice>,
}

pub struct BootBlockRegistry {
    devices: BTreeMap<String, RegisteredBlockDevice>,
    numbers: BTreeSet<BlockDeviceNumber>,
}

impl BootBlockRegistry {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            numbers: BTreeSet::new(),
        }
    }
    pub fn validate_all(
        &self,
        descriptors: &[BlockDeviceDescriptor],
    ) -> Result<(), BootBlockRegistryError> {
        for (index, descriptor) in descriptors.iter().enumerate() {
            let node = descriptor.node();
            if self.devices.contains_key(node.name().as_str()) {
                return Err(BootBlockRegistryError::DuplicateName);
            }
            if self.numbers.contains(&node.number()) {
                return Err(BootBlockRegistryError::DuplicateDeviceNumber);
            }
            for prior in &descriptors[..index] {
                if prior.node().name() == node.name() {
                    return Err(BootBlockRegistryError::DuplicateName);
                }
                if prior.node().number() == node.number() {
                    return Err(BootBlockRegistryError::DuplicateDeviceNumber);
                }
            }
        }
        Ok(())
    }

    pub fn register_all(
        &mut self,
        descriptors: &[BlockDeviceDescriptor],
    ) -> Result<(), BootBlockRegistryError> {
        self.validate_all(descriptors)?;
        for descriptor in descriptors {
            let node = descriptor.node();
            self.numbers.insert(node.number());
            self.devices.insert(
                String::from(node.name().as_str()),
                RegisteredBlockDevice {
                    device: descriptor.device().clone(),
                },
            );
        }
        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Option<Arc<dyn BlockDevice>> {
        self.devices.get(name).map(|device| device.device.clone())
    }
}

lazy_static! {
    static ref BOOT_BLOCK_REGISTRY: Mutex<BootBlockRegistry> = Mutex::new(BootBlockRegistry::new());
}

pub fn resolve_block_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    let name = name.strip_prefix("/dev/").unwrap_or(name);
    if name.is_empty() || matches!(name, "." | "..") || name.contains('/') {
        return None;
    }
    BOOT_BLOCK_REGISTRY.lock().resolve(name)
}

pub fn publish_block_descriptors(
    descriptors: &[BlockDeviceDescriptor],
) -> Result<(), BootBlockPublishError> {
    BOOT_BLOCK_REGISTRY
        .lock()
        .validate_all(descriptors)
        .map_err(BootBlockPublishError::Registry)?;
    crate::fs::dev::DEV_FS
        .add_block_devices(descriptors)
        .map_err(BootBlockPublishError::Devfs)?;
    BOOT_BLOCK_REGISTRY
        .lock()
        .register_all(descriptors)
        .map_err(BootBlockPublishError::Registry)
}

pub(crate) fn partition_name(name: &str, partno: u32) -> String {
    match name.as_bytes().last() {
        Some(byte) if byte.is_ascii_digit() => alloc::format!("{}p{}", name, partno),
        _ => alloc::format!("{}{}", name, partno),
    }
}

struct BlockMinorAllocator {
    allocated: BTreeSet<BlockDeviceNumber>,
}

impl BlockMinorAllocator {
    fn from_descriptors(descriptors: &[BlockDeviceDescriptor]) -> Self {
        Self {
            allocated: descriptors
                .iter()
                .map(|descriptor| descriptor.node().number())
                .collect(),
        }
    }

    fn allocate(&mut self, major: u64) -> Option<BlockDeviceNumber> {
        let mut minor = 0;
        loop {
            let number = BlockDeviceNumber::new(major, minor);
            if self.allocated.insert(number) {
                return Some(number);
            }
            minor = minor.checked_add(1)?;
        }
    }
}

fn boot_descriptors() -> Vec<BlockDeviceDescriptor> {
    let mut descriptors = crate::drivers::block::block_devices().to_vec();
    let mut allocator = BlockMinorAllocator::from_descriptors(&descriptors);
    let raw_devices = descriptors.clone();

    for raw_device in raw_devices {
        let probe_result = probe_mbr(raw_device.device());
        let parts = match probe_result {
            Ok(MbrProbe::Partitions(ref parts)) => {
                println!(
                    "[mbr] {} probe: {} partitions",
                    raw_device.node().name().as_str(),
                    parts.len()
                );
                parts.clone()
            }
            Ok(MbrProbe::NoMbr) => {
                println!("[mbr] {} probe: NoMbr", raw_device.node().name().as_str());
                continue;
            }
            Ok(MbrProbe::Unsupported) => {
                println!(
                    "[mbr] {} probe: Unsupported",
                    raw_device.node().name().as_str()
                );
                continue;
            }
            Err(e) => {
                println!(
                    "[mbr] {} probe: Err {:?}",
                    raw_device.node().name().as_str(),
                    e
                );
                continue;
            }
        };
        for part in parts {
            let Some(number) = allocator.allocate(raw_device.node().number().major()) else {
                println!(
                    "[mbr] no free minor for {}",
                    raw_device.node().name().as_str()
                );
                break;
            };
            let name = partition_name(raw_device.node().name().as_str(), u32::from(part.partno));
            let node = match BlockDeviceNode::new(&name, number) {
                Ok(node) => node,
                Err(_) => {
                    println!("[mbr] invalid partition name {}", name);
                    continue;
                }
            };
            let device = Arc::new(PartitionBlockDevice::new(
                raw_device.device().clone(),
                part.start_lba,
                part.sectors,
            )) as Arc<dyn BlockDevice>;
            descriptors.push(BlockDeviceDescriptor::new(node, device));
        }
    }
    descriptors
}

pub(crate) fn register_boot_block_devices() -> Result<(), BootBlockPublishError> {
    let descriptors = boot_descriptors();
    publish_block_descriptors(&descriptors)
}

pub fn mount_boot_block_devices(config: &crate::bootargs::BootConfig) {
    match register_boot_block_devices() {
        Ok(()) => {}
        Err(error) => {
            println!("[kernel] block publication failed: {:?}", error);
            return;
        }
    }

    let root = super::vfs_root();
    if config.root_from_cmdline {
        if config.root != "initramfs" && !config.root.is_empty() {
            match resolve_block_device(&config.root) {
                Some(device) => {
                    if super::mount_block_fs(&root, &device, "sdcard", "root device").is_none() {
                        println!(
                            "[initramfs] root device '{}' has no mountable filesystem",
                            config.root
                        );
                    }
                }
                None => println!("[initramfs] root device '{}' not found", config.root),
            }
        }
    } else {
        // 优先挂载 GPT/MBR 分区（如 mmcblk0p1），裸设备 block 0 是分区表头无法探测；
        // 无分区时回退到裸设备（整盘 ext4 场景）。
        let all = crate::drivers::block::block_devices();
        let raw_name = all
            .first()
            .map(|d| alloc::string::String::from(d.node().name().as_str()))
            .unwrap_or_default();
        let first_part = resolve_block_device(&partition_name(&raw_name, 1))
            .or_else(|| resolve_block_device(&partition_name(&raw_name, 0)));
        let root_device = match first_part {
            Some(part) => {
                println!(
                    "[kernel] mounting first partition of {} instead of raw device",
                    raw_name
                );
                Some(part)
            }
            None => crate::drivers::block::get_block_device(0),
        };
        match root_device {
            Some(device) => {
                if super::mount_block_fs(&root, &device, "sdcard", "root device").is_none() {
                    println!("[initramfs] root device mount failed");
                }
            }
            None => println!("[initramfs] no raw block device found"),
        }
        let tools_device =
            resolve_block_device("vdb1").or_else(|| crate::drivers::block::get_block_device(1));
        if let Some(device) = tools_device {
            if super::mount_block_fs(&root, &device, "tools", "tools disk").is_none() {
                println!("[initramfs] tools disk mount failed");
            }
        }
    }
}
