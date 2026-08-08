mod block_dev;
mod descriptor;
// `loop` 是 Rust 关键字，模块名为 loop_dev，实际文件为 loop.rs
#[path = "loop.rs"]
mod loop_dev;
pub mod partition;
mod sata_blk;
#[cfg(target_arch = "riscv64")]
pub mod dw_mshc;
#[cfg(any(feature = "block_virt", target_arch = "riscv64"))]
pub mod virtio_blk;
#[cfg(feature = "block_virt_pci")]
pub mod virtio_blk_pci;
pub mod virtio_dma_pool;
pub(crate) use block_dev::validate_block_buffer_length;
pub use block_dev::{BlockDevice, BlockDeviceError, BlockDeviceNameStyle, BlockDeviceResult};
pub use loop_dev::LoopBlockDevice;
pub use descriptor::{
    BlockDeviceDescriptor, BlockDeviceName, BlockDeviceNameError, BlockDeviceNode,
    BlockDeviceNumber,
};

use crate::hal::BLOCK_SZ;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
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

fn probe_block_devices() -> Vec<Arc<dyn BlockDevice>> {
    let mut devices = Vec::new();
    #[cfg(feature = "block_virt_pci")]
    devices.extend(virtio_blk_pci::probe_la64());
    #[cfg(any(
        target_arch = "riscv64",
        all(feature = "block_virt", not(feature = "block_virt_pci"))
    ))]
    {
        let platform_info = crate::hal::platform::platform_info();
        let device_manager = crate::hal::device::DeviceManager::new(platform_info.devices.clone());
        #[cfg(any(
            target_arch = "riscv64",
            all(feature = "block_virt", not(feature = "block_virt_pci"))
        ))]
        devices.extend(virtio_blk::probe_from_device_manager(&device_manager));
        #[cfg(target_arch = "riscv64")]
        devices.extend(dw_mshc::probe_from_device_manager(&device_manager));
    }
    #[cfg(feature = "block_sata")]
    devices.push(Arc::new(sata_blk::SataBlock::new()));
    devices
}

pub(crate) fn block_device_name(style: BlockDeviceNameStyle, index: usize) -> String {
    match style {
        BlockDeviceNameStyle::Alphabetic(prefix) => {
            const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";

            let mut remainder = index;
            let mut suffix = Vec::new();
            loop {
                suffix.push(ALPHABET[remainder % ALPHABET.len()]);
                let Some(next) = remainder
                    .checked_div(ALPHABET.len())
                    .and_then(|value| value.checked_sub(1))
                else {
                    break;
                };
                remainder = next;
            }
            suffix.reverse();

            let mut name = String::from(prefix);
            for letter in suffix {
                name.push(char::from(letter));
            }
            name
        }
        BlockDeviceNameStyle::Decimal(prefix) => alloc::format!("{}{}", prefix, index),
    }
}

fn describe_block_devices(devices: Vec<Arc<dyn BlockDevice>>) -> Vec<BlockDeviceDescriptor> {
    let mut descriptors = Vec::new();
    let mut name_counters: BTreeMap<BlockDeviceNameStyle, usize> = BTreeMap::new();
    for (index, device) in devices.into_iter().enumerate() {
        let style = device.name_style();
        let style_index = name_counters.get(&style).copied().unwrap_or(0);
        let Some(next_style_index) = style_index.checked_add(1) else {
            println!("[kernel] block device {}: name counter exhausted, skipping", index);
            continue;
        };
        name_counters.insert(style, next_style_index);
        let name = block_device_name(style, style_index);
        let Some(minor) = u64::try_from(index).ok() else {
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
        println!("[kernel] block device {}", name);
        descriptors.push(BlockDeviceDescriptor::new(node, device));
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
            get_block_device(0).unwrap_or_else(|| {
                println!("[kernel] no block device found; using dummy block device");
                Arc::new(DummyBlockDevice)
            })
        }
    };
}

pub fn block_devices() -> &'static [BlockDeviceDescriptor] {
    &BLOCK_DEVICES
}

pub fn get_block_device(index: usize) -> Option<Arc<dyn BlockDevice>> {
    BLOCK_DEVICES.get(index).map(|descriptor| descriptor.device().clone())
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
