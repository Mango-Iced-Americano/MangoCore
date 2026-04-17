#[cfg(feature= "block_virt")]
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

#[cfg(feature = "block_virt")]
lazy_static! {
    pub static ref NET_DEVICE: Arc<dyn NetDevice> = Arc::new(virtio_net::VirtIONetWrapper::new());
}