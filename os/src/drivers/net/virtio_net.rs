use super::NetDevice;
#[cfg(feature = "block_virt")]
// 借用 block 里的 VirtioHal！
use crate::drivers::block::virtio_blk::VirtioHal;
#[cfg(feature = "block_virt_pci")]
use crate::drivers::block::virtio_blk_pci::{enumerate_virtio_pci, VirtioHal};
#[cfg(feature = "block_virt")]
use crate::hal::device::DeviceManager;
#[cfg(not(feature = "block_virt"))]
use alloc::sync::Arc;
#[cfg(feature = "block_virt")]
use alloc::{sync::Arc, vec::Vec};

#[cfg(feature = "block_virt")]
use core::ptr::NonNull;
use spin::Mutex;
use virtio_drivers::device::net::{TxBuffer, VirtIONet};
#[cfg(feature = "block_virt")]
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
#[cfg(feature = "block_virt_pci")]
use virtio_drivers::transport::{pci::PciTransport, DeviceType};

// 网卡需要额外的缓冲区大小，通常 2048 足够容纳以太网最大帧
const NET_BUF_SIZE: usize = 2048;

#[cfg(feature = "block_virt")]
const VIRTIO_NET_BASE: usize = 0x10008000;
// 网卡接收队列的大小
const QUEUE_SIZE: usize = 16;

#[cfg(feature = "block_virt")]
pub struct VirtIONetWrapper(Mutex<VirtIONet<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>>);
#[cfg(feature = "block_virt_pci")]
pub struct VirtIONetWrapper(Mutex<VirtIONet<VirtioHal, PciTransport, QUEUE_SIZE>>);

#[cfg(feature = "block_virt")]
impl VirtIONetWrapper {
    pub fn new() -> Option<Self> {
        Self::try_new(VIRTIO_NET_BASE)
    }

    pub fn try_new(base_addr: usize) -> Option<Self> {
        // SAFETY: [Categories 6 and 13 — aligned access and library contract]
        // Platform device discovery supplies a mapped, page-aligned VirtIO MMIO
        // region that remains valid for the kernel lifetime.
        unsafe {
            let transport =
                MmioTransport::new(NonNull::new(base_addr as *mut VirtIOHeader)?, 0x1000).ok()?;

            // 创建网卡设备，注意这里直接把 VirtioHal 传进去了
            let net = VirtIONet::<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>::new(
                transport,
                NET_BUF_SIZE,
            )
            .ok()?;

            Some(Self(Mutex::new(net)))
        }
    }
}

/// Probe a VirtIO network device described by the platform device catalogue.
#[cfg(feature = "block_virt")]
pub fn probe_net_from_device_manager(dm: &DeviceManager) -> Option<Arc<dyn NetDevice>> {
    let mut virtio_devices: Vec<_> = dm
        .find_by_compatible("virtio,mmio")
        .into_iter()
        .filter(|device| device.mmio.is_some())
        .collect();
    virtio_devices.sort_by_key(|device| device.mmio.map(|(base, _)| base).unwrap_or(usize::MAX));

    for dev_info in virtio_devices {
        let Some((base_addr, _size)) = dev_info.mmio else {
            continue;
        };
        let Some(net_device) = VirtIONetWrapper::try_new(base_addr) else {
            continue;
        };
        return Some(Arc::new(net_device));
    }

    None
}

#[cfg(feature = "block_virt_pci")]
impl VirtIONetWrapper {
    pub fn new() -> Option<Self> {
        let transport = enumerate_virtio_pci(DeviceType::Network)?;
        let net =
            VirtIONet::<VirtioHal, PciTransport, QUEUE_SIZE>::new(transport, NET_BUF_SIZE).ok()?;

        Some(Self(Mutex::new(net)))
    }
}

impl NetDevice for VirtIONetWrapper {
    fn receive(&self, buf: &mut [u8]) -> Option<usize> {
        let mut net = self.0.lock();
        match net.receive() {
            Ok(rx_buffer) => {
                let packet = rx_buffer.packet();
                let len = packet.len();
                buf[..len].copy_from_slice(packet);
                net.recycle_rx_buffer(rx_buffer)
                    .expect("Failed to recycle rx buffer");
                Some(len)
            }
            Err(virtio_drivers::Error::NotReady) => None, // 暂无数据包
            Err(e) => panic!("Virtio Net Receive Error: {:?}", e),
        }
    }

    fn transmit(&self, buf: &[u8]) {
        let mut net = self.0.lock();
        let tx_buf = TxBuffer::from(buf);
        net.send(tx_buf).expect("Virtio Net Send Failed");
    }

    fn mac_address(&self) -> [u8; 6] {
        self.0.lock().mac_address()
    }
}
