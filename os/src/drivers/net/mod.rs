#[cfg(any(feature = "block_virt", feature = "block_virt_pci"))]
pub mod virtio_net;

pub trait NetDevice: Send + Sync {
    /// 接收一个数据包
    fn receive(&self, buf: &mut [u8]) -> Option<usize>;
    /// 发送一个数据包
    fn transmit(&self, buf: &[u8]);
    /// 获取 MAC 地址
    fn mac_address(&self) -> [u8; 6];
}

use alloc::sync::Arc;
use lazy_static::*;
use spin::Mutex;

lazy_static! {
    pub static ref NET_DEVICE: Mutex<Option<Arc<dyn NetDevice>>> = Mutex::new(None);
}

pub fn init_net_device() {
    #[cfg(any(feature = "block_virt", feature = "block_virt_pci"))]
    {
        if let Some(net_dev) = virtio_net::VirtIONetWrapper::new() {
            *NET_DEVICE.lock() = Some(Arc::new(net_dev));
        } else {
            println!("[kernel] VirtIO net device not found, skipping network init");
        }
    }
}
