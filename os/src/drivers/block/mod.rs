mod block_dev;
mod descriptor;
pub mod partition;
mod sata_blk;
#[cfg(feature = "block_virt")]
pub mod virtio_blk;
#[cfg(feature = "block_virt_pci")]
pub mod virtio_blk_pci;
pub mod virtio_dma_pool;
pub(crate) use block_dev::validate_block_buffer_length;
pub use block_dev::{BlockDevice, BlockDeviceError, BlockDeviceResult};
pub use descriptor::{
    BlockDeviceDescriptor, BlockDeviceName, BlockDeviceNameError, BlockDeviceNode,
    BlockDeviceNumber, BlockDeviceRole,
};
#[cfg(feature = "block_sata")]
type BlockDeviceImpl = sata_blk::SataBlock;
#[cfg(feature = "block_virt")]
type BlockDeviceImpl = virtio_blk::VirtIOBlock;
#[cfg(feature = "block_virt_pci")]
type BlockDeviceImpl = virtio_blk_pci::VirtIOBlock;

use crate::hal::BLOCK_SZ;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::sync::atomic::{AtomicBool, Ordering};
use lazy_static::*;

/// 标志位：跳过块设备初始化（ramfs-only 模式时由 fs::force_ramfs() 设置）
pub static SKIP_BLOCK_DEVICE: AtomicBool = AtomicBool::new(false);

/// 在 ramfs 模式下调用，阻止 BLOCK_DEVICE 初始化
pub fn disable_block_device() {
    SKIP_BLOCK_DEVICE.store(true, Ordering::Relaxed);
}

/// 虚拟块设备 — 用于 ramfs-only 模式下 BLOCK_DEVICE 的占位
struct DummyBlockDevice;
impl BlockDevice for DummyBlockDevice {
    fn read_block(&self, _block_id: usize, _buf: &mut [u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceUnavailable)
    }

    fn write_block(&self, _block_id: usize, _buf: &[u8]) -> BlockDeviceResult {
        Err(BlockDeviceError::DeviceUnavailable)
    }
}

#[cfg(all(feature = "block_virt", not(feature = "block_virt_pci")))]
fn probe_block_devices() -> Vec<Arc<dyn BlockDevice>> {
    let platform_info = crate::hal::platform::platform_info();
    let device_manager = crate::hal::device::DeviceManager::new(platform_info.devices.clone());
    virtio_blk::probe_from_device_manager(&device_manager)
}

#[cfg(feature = "block_virt_pci")]
fn probe_block_devices() -> Vec<Arc<dyn BlockDevice>> {
    virtio_blk_pci::probe_la64()
}

#[cfg(not(any(feature = "block_virt", feature = "block_virt_pci")))]
fn probe_block_devices() -> Vec<Arc<dyn BlockDevice>> {
    vec![Arc::new(BlockDeviceImpl::new())]
}

fn virtio_block_name(index: usize) -> Option<String> {
    let mut remainder = index;
    let mut suffix = Vec::new();
    loop {
        let letter = u8::try_from(remainder % 26).ok()?.checked_add(b'a')?;
        suffix.push(letter);
        let next = remainder.checked_div(26)?.checked_sub(1);
        match next {
            Some(next) => remainder = next,
            None => break,
        }
    }
    suffix.reverse();
    let suffix = core::str::from_utf8(&suffix).ok()?;
    Some(alloc::format!("vd{}", suffix))
}

fn block_device_role(index: usize) -> BlockDeviceRole {
    match index {
        0 => BlockDeviceRole::Root,
        1 => BlockDeviceRole::Tools,
        _ => BlockDeviceRole::Data,
    }
}

fn describe_block_devices(devices: Vec<Arc<dyn BlockDevice>>) -> Vec<BlockDeviceDescriptor> {
    let mut descriptors = Vec::new();
    for (index, device) in devices.into_iter().enumerate() {
        let Some(name) = virtio_block_name(index) else {
            println!("[kernel] block device {}: name generation failed, skipping", index);
            continue;
        };
        let Ok(minor) = u64::try_from(index) else {
            println!("[kernel] block device {}: minor conversion failed, skipping", name);
            continue;
        };
        let node = match BlockDeviceNode::new(&name, BlockDeviceNumber::new(254, minor)) {
            Ok(node) => node,
            Err(_) => {
                println!("[kernel] block device {}: invalid name, skipping", name);
                continue;
            }
        };
        let role = block_device_role(index);
        println!("[kernel] block device {}: {:?}", name, role);
        descriptors.push(BlockDeviceDescriptor::new(node, device, role));
    }
    descriptors
}

lazy_static! {
    pub static ref BLOCK_DEVICES: Vec<BlockDeviceDescriptor> = {
        if SKIP_BLOCK_DEVICE.load(Ordering::Relaxed) {
            println!("[kernel] block devices skipped (ramfs-only mode)");
            Vec::new()
        } else {
            describe_block_devices(probe_block_devices())
        }
    };

    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = {
        if SKIP_BLOCK_DEVICE.load(Ordering::Relaxed) {
            println!("[kernel] block device skipped (ramfs-only mode)");
            Arc::new(DummyBlockDevice)
        } else {
            block_device_by_role(BlockDeviceRole::Root)
                .unwrap_or_else(|| panic!("[kernel] FATAL: no root block device found"))
        }
    };
}

pub fn block_devices() -> &'static [BlockDeviceDescriptor] {
    &BLOCK_DEVICES
}

pub fn get_block_device(index: usize) -> Option<Arc<dyn BlockDevice>> {
    BLOCK_DEVICES.get(index).map(|descriptor| descriptor.device().clone())
}

pub fn block_device_by_role(role: BlockDeviceRole) -> Option<Arc<dyn BlockDevice>> {
    BLOCK_DEVICES
        .iter()
        .find(|descriptor| descriptor.role() == role)
        .map(|descriptor| descriptor.device().clone())
}

#[allow(unused)]
pub fn block_device_test() {
    let block_device = BLOCK_DEVICE.clone();
    let mut write_buffer = [0u8; BLOCK_SZ];
    let mut read_buffer = [0u8; BLOCK_SZ];
    for i in 0..BLOCK_SZ {
        for byte in write_buffer.iter_mut() {
            *byte = i as u8;
        }
        block_device
            .write_block(i, &write_buffer)
            .expect("block device test write failed");
        block_device
            .read_block(i, &mut read_buffer)
            .expect("block device test read failed");
        assert_eq!(write_buffer, read_buffer);
    }
    println!("block device test passed!");
}
