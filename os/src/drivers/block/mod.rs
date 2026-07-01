mod block_dev;
mod mem_blk;
pub mod partition;
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
    pub static ref BLOCK_DEVICES: [Option<Arc<dyn BlockDevice>>; 2] = {
        if SKIP_BLOCK_DEVICE.load(Ordering::Relaxed) {
            println!("[kernel] block devices skipped (ramfs-only mode)");
            [None, None]
        } else {
            probe_block_devices()
        }
    };

    /// 向后兼容别名：始终指向设备 0（官方 fs）。
    /// ramfs-only 模式下返回 DummyBlockDevice；否则要求 device 0 存在。
    pub static ref BLOCK_DEVICE: Arc<dyn BlockDevice> = {
        if SKIP_BLOCK_DEVICE.load(Ordering::Relaxed) {
            println!("[kernel] block device skipped (ramfs-only mode)");
            Arc::new(DummyBlockDevice)
        } else {
            BLOCK_DEVICES[0].clone().expect(
                "[kernel] FATAL: no block device 0 (official fs) found"
            )
        }
    };
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
