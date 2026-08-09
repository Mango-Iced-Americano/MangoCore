#[cfg(all(
    target_arch = "loongarch64",
    feature = "boot_la_uboot_dmw",
    any(feature = "gmac_probe", feature = "gmac_2k1000")
))]
pub mod gmac_2k1000;
#[cfg(all(target_arch = "riscv64", feature = "gmac_probe"))]
pub mod gmac_jh7110;
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
    /// Return the PLIC source and bounded callback used for receive work.
    fn interrupt(&self) -> Option<(usize, fn())> {
        None
    }
}

use alloc::sync::Arc;
use lazy_static::*;
use spin::Mutex;

lazy_static! {
    pub static ref NET_DEVICE: Mutex<Option<Arc<dyn NetDevice>>> = Mutex::new(None);
}

pub fn init_net_device() {
    #[cfg(all(target_arch = "riscv64", feature = "block_virt"))]
    {
        let platform_info = crate::hal::platform::platform_info();
        if !platform_info.devices.is_empty() {
            let device_manager =
                crate::hal::device::DeviceManager::new(platform_info.devices.clone());
            if let Some(net_device) = virtio_net::probe_net_from_device_manager(&device_manager) {
                publish_net_device(net_device);
                return;
            }
        }
    }

    #[cfg(all(target_arch = "riscv64", feature = "gmac_probe"))]
    {
        match gmac_jh7110::GmacJh7110::new() {
            Ok(net_device) => {
                publish_net_device(Arc::new(net_device));
                return;
            }
            Err(error) => println!("[gmac-jh7110] initialization failed: {:?}", error),
        }
    }

    #[cfg(all(
        target_arch = "loongarch64",
        feature = "boot_la_uboot_dmw",
        feature = "gmac_2k1000"
    ))]
    {
        match gmac_2k1000::Gmac2k1000::new() {
            Ok(net_dev) => publish_net_device(Arc::new(net_dev)),
            Err(error) => println!("[gmac] initialization failed: {:?}", error),
        }
    }
    #[cfg(all(
        any(feature = "block_virt", feature = "block_virt_pci"),
        not(any(
            all(
                target_arch = "loongarch64",
                feature = "boot_la_uboot_dmw",
                feature = "gmac_2k1000"
            ),
            all(target_arch = "riscv64", feature = "gmac_probe")
        ))
    ))]
    {
        #[cfg(feature = "block_virt_pci")]
        {
            if let Some(net_dev) = virtio_net::VirtIONetWrapper::new() {
                publish_net_device(Arc::new(net_dev));
            } else {
                println!("[kernel] VirtIO net device not found, skipping network init");
            }
        }
    }
}

fn publish_net_device(net_device: Arc<dyn NetDevice>) {
    #[cfg(target_arch = "riscv64")]
    let interrupt = net_device.interrupt();
    *NET_DEVICE.lock() = Some(net_device);
    #[cfg(target_arch = "riscv64")]
    if let Some((irq, handler)) = interrupt {
        if !crate::hal::arch::riscv::plic::register_handler(irq, handler) {
            println!("[net] failed to register PLIC irq {}", irq);
        }
    }
}
