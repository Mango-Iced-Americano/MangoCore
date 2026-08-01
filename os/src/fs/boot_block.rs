use super::BlockDevice;
use crate::drivers::block::partition::{probe_mbr, MbrProbe, PartitionBlockDevice};
use crate::drivers::block::{
    BlockDeviceDescriptor, BlockDeviceNode, BlockDeviceNumber, BlockDeviceRole,
};
use alloc::{collections::{BTreeMap, BTreeSet}, string::String, sync::Arc, vec::Vec};
use lazy_static::*;
use spin::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootBlockRegistryError {
    DuplicateName,
    DuplicateDeviceNumber,
    DuplicateRole,
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
    roles: BTreeMap<BlockDeviceRole, Arc<dyn BlockDevice>>,
}

impl BootBlockRegistry {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            numbers: BTreeSet::new(),
            roles: BTreeMap::new(),
        }
    }

    fn role_is_unique(role: BlockDeviceRole) -> bool {
        matches!(role, BlockDeviceRole::Root | BlockDeviceRole::Tools)
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
            if Self::role_is_unique(descriptor.role()) && self.roles.contains_key(&descriptor.role()) {
                return Err(BootBlockRegistryError::DuplicateRole);
            }
            for prior in &descriptors[..index] {
                if prior.node().name() == node.name() {
                    return Err(BootBlockRegistryError::DuplicateName);
                }
                if prior.node().number() == node.number() {
                    return Err(BootBlockRegistryError::DuplicateDeviceNumber);
                }
                if Self::role_is_unique(descriptor.role()) && prior.role() == descriptor.role() {
                    return Err(BootBlockRegistryError::DuplicateRole);
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
            if Self::role_is_unique(descriptor.role()) {
                self.roles.insert(descriptor.role(), descriptor.device().clone());
            }
        }
        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Option<Arc<dyn BlockDevice>> {
        self.devices.get(name).map(|device| device.device.clone())
    }

    pub fn resolve_role(&self, role: BlockDeviceRole) -> Option<Arc<dyn BlockDevice>> {
        self.roles.get(&role).cloned()
    }

    fn replace_role_device(&mut self, role: BlockDeviceRole, device: Arc<dyn BlockDevice>) {
        self.roles.insert(role, device);
    }
}

lazy_static! {
    static ref BOOT_BLOCK_REGISTRY: Mutex<BootBlockRegistry> = Mutex::new(BootBlockRegistry::new());
}

pub fn resolve_block_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    BOOT_BLOCK_REGISTRY.lock().resolve(name)
}

pub fn resolve_role_block_device(role: BlockDeviceRole) -> Option<Arc<dyn BlockDevice>> {
    BOOT_BLOCK_REGISTRY.lock().resolve_role(role)
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

fn partition_name(name: &str, partno: u32) -> String {
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

fn boot_descriptors() -> (Vec<BlockDeviceDescriptor>, Option<Arc<dyn BlockDevice>>) {
    let mut descriptors = crate::drivers::block::block_devices().to_vec();
    let mut allocator = BlockMinorAllocator::from_descriptors(&descriptors);
    let mut tools_mount = None;
    let raw_devices = descriptors.clone();

    for raw_device in raw_devices {
        if raw_device.role() != BlockDeviceRole::Tools {
            continue;
        }
        let parts = match probe_mbr(raw_device.device()) {
            Ok(MbrProbe::Partitions(parts)) => parts,
            Ok(MbrProbe::NoMbr | MbrProbe::Unsupported) | Err(_) => continue,
        };
        for part in parts {
            let Some(number) = allocator.allocate(raw_device.node().number().major()) else {
                println!("[mbr] no free minor for {}", raw_device.node().name().as_str());
                break;
            };
            let name = partition_name(
                raw_device.node().name().as_str(),
                u32::from(part.partno),
            );
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
            if part.partno == 1 {
                tools_mount = Some(device.clone());
            }
            descriptors.push(BlockDeviceDescriptor::new(node, device, BlockDeviceRole::Data));
        }
    }
    (descriptors, tools_mount)
}

pub(crate) fn register_boot_block_devices() -> Result<(), BootBlockPublishError> {
    let (descriptors, tools_mount) = boot_descriptors();
    publish_block_descriptors(&descriptors)?;
    if let Some(tools_mount) = tools_mount {
        BOOT_BLOCK_REGISTRY
            .lock()
            .replace_role_device(BlockDeviceRole::Tools, tools_mount);
    }
    Ok(())
}

pub fn mount_tools_disk() {
    let Some(tools_device) = resolve_role_block_device(BlockDeviceRole::Tools) else {
        println!("[kernel] no tools disk found, skipping /tools mount");
        return;
    };
    let root = super::vfs_root();
    let _ = super::mount_block_fs(&root, &tools_device, "tools", "tools disk");
}

pub fn mount_boot_block_devices(config: &crate::bootargs::BootConfig) {
    if let Err(error) = register_boot_block_devices() {
        println!("[kernel] block publication failed: {:?}", error);
        return;
    }

    if config.root == "initramfs" {
        mount_tools_disk();
        return;
    }

    let root = super::vfs_root();
    let root_name = config.root.strip_prefix("/dev/").unwrap_or(&config.root);
    let root_device = resolve_block_device(root_name);
    match root_device {
        Some(device) => {
            if super::mount_block_fs(&root, &device, "sdcard", "root device").is_none() {
                println!("[initramfs] root device '{}' mount failed", config.root);
            }
        }
        None => println!("[initramfs] root device '{}' not found", config.root),
    }

    if let Some(tools_device) = resolve_role_block_device(BlockDeviceRole::Tools) {
        if super::mount_block_fs(&root, &tools_device, "tools", "tools disk").is_none() {
            println!("[initramfs] tools disk mount failed");
        }
    }
}
