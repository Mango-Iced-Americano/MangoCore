mod block_dev;
pub mod partition;
mod sata_blk;
pub mod virtio_dma_pool;
#[cfg(feature = "block_virt")]
pub mod virtio_blk;
#[cfg(feature = "block_virt_pci")]
pub mod virtio_blk_pci;
pub use block_dev::BlockDevice;
#[cfg(feature = "block_sata")]
type BlockDeviceImpl = sata_blk::SataBlock;
#[cfg(feature = "block_virt")]
type BlockDeviceImpl = virtio_blk::VirtIOBlock;
#[cfg(feature = "block_virt_pci")]
type BlockDeviceImpl = virtio_blk_pci::VirtIOBlock;

use crate::hal::BLOCK_SZ;
use alloc::sync::Arc;
use lazy_static::*;

// ── 平台相关的块设备探测 ──

#[cfg(all(feature = "block_virt", not(feature = "block_virt_pci")))]
fn probe_block_devices() -> [Option<Arc<dyn BlockDevice>>; 2] {
    virtio_blk::probe_rv64()
}

#[cfg(feature = "block_virt_pci")]
fn probe_block_devices() -> [Option<Arc<dyn BlockDevice>>; 2] {
    virtio_blk_pci::probe_la64()
}

#[cfg(not(any(feature = "block_virt", feature = "block_virt_pci")))]
fn probe_block_devices() -> [Option<Arc<dyn BlockDevice>>; 2] {
    // 内存盘 / SATA：单设备，slot1 为空
    [Some(Arc::new(BlockDeviceImpl::new())), None]
}

lazy_static! {
    /// 多块设备数组。索引 0 = 官方 fs (x0)，索引 1 = 工具盘 (x1)。
    /// 每个条目在设备未探测到时为 None。
    pub static ref BLOCK_DEVICES: [Option<Arc<dyn BlockDevice>>; 2] = probe_block_devices();

    /// 向后兼容别名：始终指向设备 0（官方 fs）。
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = BLOCK_DEVICES[0]
        .clone()
        .expect("[kernel] FATAL: no block device 0 (official fs) found");
}

/// 返回块设备数组的只读引用
pub fn block_devices() -> &'static [Option<Arc<dyn BlockDevice>>; 2] {
    &BLOCK_DEVICES
}

/// 获取指定索引的块设备（存在时返回 Some）
pub fn get_block_device(index: usize) -> Option<Arc<dyn BlockDevice>> {
    BLOCK_DEVICES.get(index).and_then(|dev| dev.clone())
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
