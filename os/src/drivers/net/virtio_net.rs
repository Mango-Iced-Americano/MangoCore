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
#[cfg(feature = "block_virt")]
use virtio_drivers::transport::{DeviceType, Transport};
#[cfg(feature = "block_virt_pci")]
use virtio_drivers::transport::{pci::PciTransport, DeviceType};

// 网卡需要额外的缓冲区大小，通常 2048 足够容纳以太网最大帧
const NET_BUF_SIZE: usize = 2048;

// 网卡接收队列的大小
const QUEUE_SIZE: usize = 16;

#[cfg(feature = "block_virt")]
use core::convert::TryInto;
#[cfg(feature = "block_virt")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "block_virt")]
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = 0x60;
#[cfg(feature = "block_virt")]
const VIRTIO_MMIO_INTERRUPT_ACK: usize = 0x64;
#[cfg(feature = "block_virt")]
static VIRTIO_NET_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "block_virt")]
pub struct VirtIONetWrapper {
    net: Mutex<VirtIONet<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>>,
    irq: Option<usize>,
}
#[cfg(feature = "block_virt_pci")]
pub struct VirtIONetWrapper {
    net: Mutex<VirtIONet<VirtioHal, PciTransport, QUEUE_SIZE>>,
}

#[cfg(feature = "block_virt")]
impl VirtIONetWrapper {
    pub fn try_new(base_addr: usize, irq: Option<usize>) -> Option<Self> {
        // SAFETY: [Categories 6 and 13 — aligned access and library contract]
        // Platform device discovery supplies a mapped, page-aligned VirtIO MMIO
        // region that remains valid for the kernel lifetime.
        unsafe {
            let transport =
                MmioTransport::new(NonNull::new(base_addr as *mut VirtIOHeader)?, 0x1000).ok()?;

            if transport.device_type() != DeviceType::Network {
                // `MmioTransport::drop` resets the device. This transport only read the
                // immutable device ID while filtering the FDT catalogue, so it must not
                // reset a device owned by another driver.
                core::mem::forget(transport);
                return None;
            }

            // 创建网卡设备，注意这里直接把 VirtioHal 传进去了
            let mut net = VirtIONet::<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>::new(
                transport,
                NET_BUF_SIZE,
            )
            .ok()?;
            net.enable_interrupts();
            VIRTIO_NET_MMIO_BASE.store(base_addr, Ordering::Release);

            Some(Self {
                net: Mutex::new(net),
                irq,
            })
        }
    }
}

/// Probe a VirtIO network device described by the platform device catalogue.
#[cfg(feature = "block_virt")]
pub fn probe_net_from_device_manager(dm: &DeviceManager) -> Option<Arc<dyn NetDevice>> {
    let mut virtio_devices: Vec<_> = dm
        .find_enabled_by_compatible("virtio,mmio")
        .into_iter()
        .filter(|device| device.mmio_range(0).is_some())
        .collect();
    virtio_devices
        .sort_by_key(|device| device.mmio_range(0).map(|range| range.base).unwrap_or(usize::MAX));

    for dev_info in virtio_devices {
        let Some(range) = dev_info.mmio_range(0) else {
            continue;
        };
        let base_addr = range.base;
        let irq = device_interrupt(dev_info);
        let Some(net_device) = VirtIONetWrapper::try_new(base_addr, irq) else {
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

            Some(Self {
                net: Mutex::new(net),
            })
    }
}

impl NetDevice for VirtIONetWrapper {
    fn receive(&self, buf: &mut [u8]) -> Option<usize> {
        let mut net = self.net.lock();
        let _bridge = crate::drivers::block::virtio_dma_pool::dma_bridge_lock();
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
        let mut net = self.net.lock();
        let _bridge = crate::drivers::block::virtio_dma_pool::dma_bridge_lock();
        let tx_buf = TxBuffer::from(buf);
        net.send(tx_buf).expect("Virtio Net Send Failed");
    }

    fn mac_address(&self) -> [u8; 6] {
        self.net.lock().mac_address()
    }

    #[cfg(feature = "block_virt")]
    fn interrupt(&self) -> Option<(usize, fn())> {
        self.irq.map(|irq| (irq, virtio_net_irq as fn()))
    }
}

#[cfg(feature = "block_virt")]
fn device_interrupt(device: &crate::hal::platform::DeviceInfo) -> Option<usize> {
    let bytes: [u8; 4] = device.raw_property("interrupts").ok()?.get(..4)?.try_into().ok()?;
    let irq = u32::from_be_bytes(bytes) as usize;
    (irq != 0).then_some(irq)
}

#[cfg(feature = "block_virt")]
fn virtio_net_irq() {
    let base = VIRTIO_NET_MMIO_BASE.load(Ordering::Acquire);
    if base != 0 {
        // SAFETY: `try_new` publishes only the FDT-validated, identity-mapped
        // VirtIO MMIO base after queue setup; both registers are aligned u32s.
        let status = unsafe {
            core::ptr::read_volatile((base + VIRTIO_MMIO_INTERRUPT_STATUS) as *const u32)
        };
        if status != 0 {
            // SAFETY: this is the paired VirtIO interrupt-ack register in the
            // same validated MMIO page; acknowledgement is a single W1C store.
            unsafe {
                core::ptr::write_volatile((base + VIRTIO_MMIO_INTERRUPT_ACK) as *mut u32, status)
            };
        }
    }
    crate::net::config::NET_INTERFACE.notify_rx_interrupt();
}
