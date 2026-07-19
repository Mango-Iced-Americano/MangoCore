#[cfg(all(
    target_arch = "loongarch64",
    feature = "board_2k1000",
    any(feature = "gmac_probe", feature = "gmac_2k1000")
))]
pub mod gmac_2k1000;
pub mod veth;
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
    #[cfg(all(
        target_arch = "loongarch64",
        feature = "board_2k1000",
        feature = "gmac_2k1000"
    ))]
    {
        match gmac_2k1000::Gmac2k1000::new() {
            Ok(net_dev) => *NET_DEVICE.lock() = Some(Arc::new(net_dev)),
            Err(error) => println!("[gmac] initialization failed: {:?}", error),
        }
    }
    #[cfg(all(
        any(feature = "block_virt", feature = "block_virt_pci"),
        not(all(
            target_arch = "loongarch64",
            feature = "board_2k1000",
            feature = "gmac_2k1000"
        ))
    ))]
    {
        if let Some(net_dev) = virtio_net::VirtIONetWrapper::new() {
            *NET_DEVICE.lock() = Some(Arc::new(net_dev));
        } else {
            println!("[kernel] VirtIO net device not found, skipping network init");
        }
    }
}
