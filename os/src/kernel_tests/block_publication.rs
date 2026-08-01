use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::drivers::block::{
    BlockDevice, BlockDeviceDescriptor, BlockDeviceError, BlockDeviceNode, BlockDeviceNumber,
    BlockDeviceResult, BlockDeviceRole,
};
use crate::fs::dev::{mkdev, DevFS};
use crate::fs::boot_block::{BootBlockRegistry, BootBlockRegistryError};
use crate::fs::vfs::{FileSystem as _, IndexNode as _};
use crate::kernel_tests::runner::KernelTest;

pub fn tests() -> Vec<KernelTest> {
    vec![
        KernelTest::new(
            "block_publication::publishes_names_independent_of_descriptor_order",
            test_publishes_names_independent_of_descriptor_order,
        ),
        KernelTest::new(
            "block_publication::rejects_duplicate_names_and_device_numbers",
            test_rejects_duplicate_names_and_device_numbers,
        ),
        KernelTest::new(
            "block_publication::devfs_uses_descriptor_name_and_device_number",
            test_devfs_uses_descriptor_name_and_device_number,
        ),
    ]
}

struct TestBlockDevice;

impl BlockDevice for TestBlockDevice {
    fn read_block(&self, _block_id: usize, _buf: &mut [u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceUnavailable)
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceUnavailable)
    }
}

fn test_publishes_names_independent_of_descriptor_order() -> Result<(), &'static str> {
    // Given: named root and tools descriptors in reverse order.
    let root = descriptor("vda", 0, BlockDeviceRole::Root)?;
    let tools = descriptor("vdb", 1, BlockDeviceRole::Tools)?;
    let mut registry = BootBlockRegistry::new();

    // When: the registry publishes both descriptors.
    registry
        .register_all(&[tools, root])
        .map_err(|_| "descriptor publication failed")?;
    // Then: lookup follows names and roles rather than slots.

    if registry.resolve("vda").is_none() || registry.resolve("vdb").is_none() {
        return Err("named descriptors were not resolvable by their published names");
    }
    if registry.resolve_role(BlockDeviceRole::Root).is_none() {
        return Err("root role did not resolve independently of descriptor order");
    }
    if registry.resolve_role(BlockDeviceRole::Tools).is_none() {
        return Err("tools role did not resolve independently of descriptor order");
    }
    Ok(())
}

fn test_rejects_duplicate_names_and_device_numbers() -> Result<(), &'static str> {
    // Given: descriptors with colliding names and device numbers.
    let first = descriptor("vda", 0, BlockDeviceRole::Root)?;
    let same_name = descriptor("vda", 1, BlockDeviceRole::Data)?;
    let same_number = descriptor("vdb", 0, BlockDeviceRole::Tools)?;
    let mut registry = BootBlockRegistry::new();

    // When: registration validates both descriptor sets.
    // Then: collisions are rejected before publication.
    if registry.register_all(&[first.clone(), same_name])
        != Err(BootBlockRegistryError::DuplicateName)
    {
        return Err("duplicate published name was accepted");
    }
    if registry.register_all(&[first, same_number])
        != Err(BootBlockRegistryError::DuplicateDeviceNumber)
    {
        return Err("duplicate major/minor device number was accepted");
    }
    Ok(())
}

fn test_devfs_uses_descriptor_name_and_device_number() -> Result<(), &'static str> {
    // Given: one descriptor with a caller-selected major/minor number.
    let descriptor = descriptor("vdc", 7, BlockDeviceRole::Data)?;
    let devfs = DevFS::new();
    // When: generic devfs publication receives the descriptor.
    devfs
        .add_block_devices(&[descriptor])
        .map_err(|_| "descriptor-backed devfs publication failed")?;
    // Then: lookup and metadata use the descriptor name and number.
    let node = devfs
        .root_inode()
        .find("vdc")
        .map_err(|_| "devfs did not publish the descriptor name")?;
    if node
        .metadata()
        .map_err(|_| "devfs node has no metadata")?
        .raw_dev
        != mkdev(254, 7)
    {
        return Err("devfs node ignored the descriptor major/minor number");
    }
    Ok(())
}

fn descriptor(
    name: &str,
    minor: u64,
    role: BlockDeviceRole,
) -> Result<BlockDeviceDescriptor, &'static str> {
    Ok(BlockDeviceDescriptor::new(
        BlockDeviceNode::new(name, BlockDeviceNumber::new(254, minor))
            .map_err(|_| "test block node was rejected")?,
        Arc::new(TestBlockDevice),
        role,
    ))
}
