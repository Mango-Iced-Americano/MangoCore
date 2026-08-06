use super::NetDevice;
#[cfg(feature = "block_virt")]
// 借用 block 里的 VirtioHal！
use crate::drivers::block::virtio_blk::VirtioHal;
#[cfg(feature = "block_virt_pci")]
use crate::drivers::block::virtio_blk_pci::{enumerate_virtio_pci, VirtioHal};

#[cfg(feature = "block_virt")]
use core::ptr::NonNull;
use core::sync::atomic::Ordering;
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
        unsafe {
            let transport = MmioTransport::new(
                NonNull::new_unchecked(VIRTIO_NET_BASE as *mut VirtIOHeader),
                0x1000,
            )
            .ok()?;

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
        // 生产级断言：物理发送只允许发生在 CPU0 worker 的 IRQ-on 物理扫描窗口内。
        //
        // VirtIO `net.send` 是同步发送，在 used-ring 耗尽时会自旋等待完成中断；
        // 若在 IRQ-off 上下文（syscall/trap/boot probe）调用，completion 永远到
        // 达不了，整个内核死锁。CPU0 worker 通过 `with_local_interrupts_enabled`
        // 开窗，`physical_poll_active` 标记窗口状态；三条条件同时成立才允许发送。
        assert!(
            crate::hal::irq_enabled()
                && crate::smp::cpu_id() == crate::smp::BOOT_CPU_ID
                && crate::net::config::NET_INTERFACE.physical_poll_active(),
            "VirtIO transmit outside CPU0 IRQ-on physical poll window \
             (irq_enabled={} cpu={})",
            crate::hal::irq_enabled(),
            crate::smp::cpu_id(),
        );
        crate::net::config::VIRTIO_TX_ENTER.fetch_add(1, Ordering::Relaxed);
        let mut net = self.0.lock();
        let tx_buf = TxBuffer::from(buf);
        net.send(tx_buf).expect("Virtio Net Send Failed");
        crate::net::config::VIRTIO_TX_COMPLETE.fetch_add(1, Ordering::Relaxed);
        crate::net::config::VIRTIO_TX_CPU_MASK
            .fetch_or(1usize << crate::smp::cpu_id(), Ordering::Relaxed);
        crate::net::config::VIRTIO_TX_BYTES.fetch_add(buf.len() as u64, Ordering::Relaxed);
    }

    fn mac_address(&self) -> [u8; 6] {
        self.0.lock().mac_address()
    }
}
