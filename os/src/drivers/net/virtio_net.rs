use super::NetDevice;
// 借用 block 里的 VirtioHal！
use crate::drivers::block::virtio_blk::VirtioHal;

use alloc::vec::Vec;
use core::ptr::NonNull;
use spin::Mutex;
use virtio_drivers::device::net::{VirtIONet,TxBuffer,RxBuffer};
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};

// 网卡需要额外的缓冲区大小，通常 2048 足够容纳以太网最大帧
const NET_BUF_SIZE: usize = 2048;

const VIRTIO_NET_BASE: usize = 0x10008000;
// 网卡接收队列的大小
const QUEUE_SIZE: usize = 16;

pub struct VirtIONetWrapper(Mutex<VirtIONet<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>>);

impl VirtIONetWrapper {
    pub fn new() -> Self {
        unsafe {
            let transport = MmioTransport::new(
                NonNull::new_unchecked(VIRTIO_NET_BASE as *mut VirtIOHeader),
                0x1000,
            ).expect("virtio net transport initialization failed");

            // 创建网卡设备，注意这里直接把 VirtioHal 传进去了
            let net = VirtIONet::<VirtioHal, MmioTransport<'static>, QUEUE_SIZE>::new(
                transport,
                NET_BUF_SIZE,
            ).expect("virtio net device initialization failed");

            Self(Mutex::new(net))
        }
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
                net.recycle_rx_buffer(rx_buffer).expect("Failed to recycle rx buffer");
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