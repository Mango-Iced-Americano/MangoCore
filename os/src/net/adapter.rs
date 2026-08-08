use crate::drivers::net::veth::VethDriver;
use crate::drivers::net::NetDevice;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{Device, DeviceCapabilities, Loopback, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use spin::Mutex;

/// Packets produced while the local interrupt bit is off cannot synchronously
/// wait for a VirtIO used-ring completion. Keep the device Arc with the packet
/// so the queue is independent of any DeviceStack lock and can be drained by
/// the scheduler-context network poll.
static DEFERRED_TX_QUEUE: Mutex<Vec<(Arc<dyn NetDevice>, Vec<u8>)>> = Mutex::new(Vec::new());
const DEFERRED_TX_MAX_PACKETS: usize = 64;

pub(crate) fn drain_deferred_tx() {
    if !crate::hal::irq_enabled() {
        return;
    }
    let packets = core::mem::take(&mut *DEFERRED_TX_QUEUE.lock());
    for (device, packet) in packets {
        device.transmit(&packet);
    }
}

/// 单设备 smoltcp 适配器。每个 DeviceStack 只拥有一个设备实例，loopback、
/// 物理网卡和 veth 之间不再通过共享路由缓冲区转发。
pub enum IfaceDevice {
    Lo(Loopback),
    Eth(SmoltcpDeviceAdapter),
    Veth(VethDriver),
}

pub enum IfaceRxToken<'a> {
    Lo(<Loopback as Device>::RxToken<'a>),
    Eth(<SmoltcpDeviceAdapter as Device>::RxToken<'a>),
    Veth(<VethDriver as Device>::RxToken<'a>),
}

pub enum IfaceTxToken<'a> {
    Lo(<Loopback as Device>::TxToken<'a>),
    Eth(<SmoltcpDeviceAdapter as Device>::TxToken<'a>),
    Veth(<VethDriver as Device>::TxToken<'a>),
}

impl Device for IfaceDevice {
    type RxToken<'a> = IfaceRxToken<'a>;
    type TxToken<'a> = IfaceTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        match self {
            Self::Lo(lo) => lo.capabilities(),
            Self::Eth(eth) => eth.capabilities(),
            Self::Veth(veth) => veth.capabilities(),
        }
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self {
            Self::Lo(lo) => {
                let (rx, tx) = lo.receive(timestamp)?;
                Some((IfaceRxToken::Lo(rx), IfaceTxToken::Lo(tx)))
            }
            Self::Eth(eth) => {
                let (rx, tx) = eth.receive(timestamp)?;
                Some((IfaceRxToken::Eth(rx), IfaceTxToken::Eth(tx)))
            }
            Self::Veth(veth) => {
                let (rx, tx) = veth.receive(timestamp)?;
                Some((IfaceRxToken::Veth(rx), IfaceTxToken::Veth(tx)))
            }
        }
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        match self {
            Self::Lo(lo) => lo.transmit(timestamp).map(IfaceTxToken::Lo),
            Self::Eth(eth) => eth.transmit(timestamp).map(IfaceTxToken::Eth),
            Self::Veth(veth) => veth.transmit(timestamp).map(IfaceTxToken::Veth),
        }
    }
}

impl<'a> RxToken for IfaceRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Lo(t) => t.consume(f),
            Self::Eth(t) => t.consume(f),
            Self::Veth(t) => t.consume(f),
        }
    }
}

impl<'a> TxToken for IfaceTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Lo(t) => t.consume(len, f),
            Self::Eth(t) => t.consume(len, f),
            Self::Veth(t) => t.consume(len, f),
        }
    }
}

/// No-op network device used when no physical NIC is present.
/// Allows the smoltcp stack to function with loopback only.
pub struct NullNetDevice;

impl NetDevice for NullNetDevice {
    fn receive(&self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
    fn transmit(&self, _buf: &[u8]) {
        // No-op: packets sent to eth on a null device are silently dropped
    }
    fn mac_address(&self) -> [u8; 6] {
        // Locally administered unicast MAC (not all-zero, required by smoltcp DHCP)
        [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]
    }
}

pub struct SmoltcpDeviceAdapter {
    pub inner: Arc<dyn NetDevice>,
}

impl SmoltcpDeviceAdapter {
    pub fn new(inner: Arc<dyn NetDevice>) -> Self {
        Self { inner }
    }
}

impl Device for SmoltcpDeviceAdapter {
    type RxToken<'a> = NetRxToken;
    type TxToken<'a> = NetTxToken;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = [0u8; 2048];

        if let Some(len) = self.inner.receive(&mut buf) {
            crate::task::perf::record_net_rx(len);
            let packet = buf[..len].to_vec();
            let rx = NetRxToken { buf: packet };
            let tx = NetTxToken {
                inner: self.inner.clone(),
            };
            Some((rx, tx))
        } else {
            None
        }
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(NetTxToken {
            inner: self.inner.clone(),
        })
    }
}

pub struct NetRxToken {
    buf: Vec<u8>,
}

impl RxToken for NetRxToken {
    /// 消费收到的网络数据包。
    ///
    /// # Semantics
    ///
    /// 在将数据包传递给 smoltcp 处理前，先尝试捕获 ARP 回复更新邻居表
    /// （`try_capture_arp_reply`）。
    ///
    /// # Locking
    ///
    /// `try_capture_arp_reply` 读取 `CURRENT_POLL_IFINDEX`（Mutex），
    /// 并可能获取邻居表锁写入 ARP 映射。此操作在 smoltcp Rx token
    /// 的闭包内执行，闭包由 smoltcp 的 `Interface::poll` 调用——
    /// 应保持简短，避免额外的网络 I/O。
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let ifindex = *crate::net::neighbour::CURRENT_POLL_IFINDEX.lock();
        crate::net::neighbour::try_capture_arp_reply(&self.buf, ifindex);
        f(&mut self.buf)
    }
}

pub struct NetTxToken {
    inner: Arc<dyn NetDevice>,
}

impl TxToken for NetTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // 防空包
        if len == 0 {
            log::warn!("[NetTxToken] Attempted to send a zero-length packet, intercepting.");
            let mut empty_buf = [];
            return f(&mut empty_buf);
        }

        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        crate::task::perf::record_net_tx_submit(len);
        if crate::hal::irq_enabled() {
            self.inner.transmit(&buf);
        } else {
            let mut queue = DEFERRED_TX_QUEUE.lock();
            if queue.len() >= DEFERRED_TX_MAX_PACKETS {
                queue.remove(0);
                crate::task::perf::record_net_tx_drop();
            }
            queue.push((self.inner, buf));
        }
        result
    }
}
