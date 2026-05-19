mod block_dev;
mod mem_blk;
mod sata_blk;
#[cfg(feature = "block_virt")]
pub mod virtio_blk;
#[cfg(feature = "block_virt_pci")]
pub mod virtio_blk_pci;
pub use block_dev::BlockDevice;
#[cfg(feature = "block_mem")]
type BlockDeviceImpl = mem_blk::MemBlockWrapper;
#[cfg(feature = "block_sata")]
type BlockDeviceImpl = sata_blk::SataBlock;
#[cfg(feature = "block_virt")]
type BlockDeviceImpl = virtio_blk::VirtIOBlock;
#[cfg(feature = "block_virt_pci")]
type BlockDeviceImpl = virtio_blk_pci::VirtIOBlock;

use crate::hal::BLOCK_SZ;
use alloc::sync::Arc;
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
    fn read_block(&self, _block_id: usize, _buf: &mut [u8]) {
        panic!("DummyBlockDevice::read_block called — block device is disabled (ramfs-only mode)");
    }
    fn write_block(&self, _block_id: usize, _buf: &[u8]) {
        panic!("DummyBlockDevice::write_block called — block device is disabled (ramfs-only mode)");
    }
}

lazy_static! {
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = {
        if SKIP_BLOCK_DEVICE.load(Ordering::Relaxed) {
            println!("[kernel] block device skipped (ramfs-only mode)");
            Arc::new(DummyBlockDevice)
        } else {
            Arc::new(BlockDeviceImpl::new())
        }
    };
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
        block_device.write_block(i as usize, &write_buffer);
        block_device.read_block(i as usize, &mut read_buffer);
        assert_eq!(write_buffer, read_buffer);
    }
    println!("block device test passed!");
}
