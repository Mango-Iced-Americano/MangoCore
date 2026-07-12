use core::convert::TryInto;
use core::result;

use crate::drivers::net::veth::VethDriver;
use crate::drivers::net::NetDevice;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{Device, DeviceCapabilities, Loopback, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress, Ipv4Address, Ipv4Packet,
};
use spin::Mutex;

/// Single-device wrapper enum — replaces RoutingDevice's multi-device software switch.
/// Each DeviceStack gets its own IfaceDevice (Loopback for lo, Ethernet for eth0).
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
/// transmit() always returns Some to keep RoutingDevice::transmit() working.
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

/// 静态路由缓冲区，`RoutingTxToken::consume` 独用。
///
/// # Safety
///
/// **单核假设**：MangoCore 是单核抢占式内核。此静态缓冲区仅被
/// `RoutingTxToken::consume` 在 `transmit` 令牌生效期间访问，且 `transmit`
/// 令牌是 exclusive 的（smoltcp `TxToken` 语义保证一次只有一个活跃令牌）。
/// 因此不存在竞态。
///
/// 若未来支持多核并行轮询网卡，需改为 per-core 或 per-stack 分配。
static mut ROUTING_BUF: [u8; 65536] = [0u8; 65536];
pub struct RoutingDevice {
    pub eth: SmoltcpDeviceAdapter,
    pub lo: Loopback,
    pub hw_addr: EthernetAddress,
}

impl RoutingDevice {
    pub fn new(eth: SmoltcpDeviceAdapter, lo: Loopback) -> Self {
        let mac = eth.inner.mac_address();
        let hw_addr = EthernetAddress(mac);
        Self { eth, lo, hw_addr }
    }
}

impl Device for RoutingDevice {
    type RxToken<'a> = RoutingRxToken<'a>;
    type TxToken<'a> = RoutingTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = self.eth.capabilities();
        caps.medium = Medium::Ethernet;
        // caps.max_transmission_unit = 65535; // 支持更大的 MTU 以适应环回接口

        //一定得是1514,别问为什么
        caps.max_transmission_unit = 1514; // 以太网 MTU，环回接口也遵循这个限制

        caps
    }

    fn receive(&mut self, timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // 1. 优先收环回包
        if let Some((rx, tx)) = self.lo.receive(timestamp) {
            return Some((RoutingRxToken::Lo(rx), RoutingTxToken::Lo(tx)));
        }
        // 2. 其次收物理包
        if let Some((rx, tx)) = self.eth.receive(timestamp) {
            return Some((RoutingRxToken::Eth(rx), RoutingTxToken::Eth(tx)));
        }
        None
    }

    fn transmit(&mut self, timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(RoutingTxToken::Mixed {
            eth_tx: self.eth.transmit(timestamp)?,
            lo_tx: self.lo.transmit(timestamp)?,
            hw_addr: self.hw_addr,
        })
    }
}

pub enum RoutingRxToken<'a> {
    Eth(<SmoltcpDeviceAdapter as Device>::RxToken<'a>),
    Lo(<Loopback as Device>::RxToken<'a>),
}

impl<'a> RxToken for RoutingRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Eth(t) => t.consume(f),
            Self::Lo(t) => t.consume(f),
        }
    }
}

pub enum RoutingTxToken<'a> {
    Eth(<SmoltcpDeviceAdapter as Device>::TxToken<'a>),
    Lo(<Loopback as Device>::TxToken<'a>),
    Mixed {
        eth_tx: <SmoltcpDeviceAdapter as Device>::TxToken<'a>,
        lo_tx: <Loopback as Device>::TxToken<'a>,
        hw_addr: EthernetAddress,
    },
}

impl<'a> TxToken for RoutingTxToken<'a> {
    /// 路由传输令牌：根据目标 MAC 或 IP 将帧分发到 eth/lo 设备。
    ///
    /// # Semantics
    ///
    /// 解析 `EthernetFrame` 的 `dst_addr`：
    /// - 发给自己的 MAC → 仅环回 (lo)
    /// - 广播包 → 环回 + 直通 (lo + eth)
    /// - 其他 → 直通 (eth)
    ///
    /// 另外检查 IP 目的地址：若目标 IP 是回环或本地地址，覆盖为仅环回。
    ///
    /// # Locking
    ///
    /// 调用 `crate::net::net_core::current_netns().device_list.lock()`
    /// 检查本地 IP 匹配，并间接获取接口信息。此锁在快速路径上获取，
    /// 但 token consume 始终在 smoltcp poll 的闭包内，不应与其他路径竞争。
    ///
    /// # Safety
    ///
    /// `ROUTING_BUF` 是 `static mut`——仅被此函数访问，且此函数以
    /// smoltcp `TxToken` 的 exclusive 语义执行（单次 poll 中只有一个
    /// token 活跃）。若未来多核并行 poll，需要重新评估。
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Eth(t) => t.consume(len, f),
            Self::Lo(t) => t.consume(len, f),
            Self::Mixed {
                eth_tx,
                lo_tx,
                hw_addr,
            } => {
                // Safety: `ROUTING_BUF` 是 64KB 的 static mut 缓冲区，在此
                // smoltcp `TxToken` consume 路径中是 exclusive 的（单核，单次
                // poll 单活跃令牌）。`len` 在 smoltcp 上层由 `max_transmission_unit`
                // 受限于 1514 字节，远小于 64KB，不会越界。
                let mut buf = unsafe { &mut ROUTING_BUF[..len] };

                let res = f(&mut buf);

                let mut send_to_lo = false;
                let mut send_to_eth = false;

                // Routing
                if let Ok(frame) = EthernetFrame::new_checked(&buf) {
                    let dst_mac = frame.dst_addr();
                    let is_broadcast = dst_mac.is_broadcast();

                    let hw_addr = hw_addr;

                    if dst_mac == hw_addr {
                        // 单播给自己的 MAC，只走环回
                        send_to_lo = true;
                    } else if is_broadcast {
                        // 广播包（如查询未知 IP 的 ARP），既要问外网，也要问自己
                        send_to_lo = true;
                        send_to_eth = true;
                    } else {
                        // 发给其他机器的单播，默认走以太网
                        send_to_eth = true;
                    }

                    // Local delivery check: route loopback or own IP via lo, external via eth
                    match frame.ethertype() {
                        EthernetProtocol::Ipv4 => {
                            if let Ok(ipv4) = Ipv4Packet::new_checked(frame.payload()) {
                                let dst_addr = ipv4.dst_addr();
                                let dst_ip = IpAddress::Ipv4(dst_addr);
                                let is_loopback = dst_addr.as_bytes()[0] == 127;
                                let is_local = crate::net::net_core::current_netns()
                                    .device_list
                                    .lock()
                                    .values()
                                    .any(|iface| {
                                        iface.ip_addrs().iter().any(|c| c.address() == dst_ip)
                                    });
                                if is_loopback || is_local {
                                    log::debug!(
                                        "[RoutingTxToken] dst={} -> local delivery (lo)",
                                        dst_addr
                                    );
                                    send_to_lo = true;
                                    send_to_eth = false;
                                } else {
                                    send_to_lo = false;
                                    send_to_eth = true;
                                }
                            }
                        }
                        EthernetProtocol::Arp => {
                            if let Ok(arp) = ArpPacket::new_checked(frame.payload()) {
                                let target_ip = arp.target_protocol_addr();
                                let ipv4 = Ipv4Address::from_bytes(
                                    &target_ip[..4].try_into().unwrap_or([0; 4]),
                                );
                                let dst_ip = IpAddress::Ipv4(ipv4);
                                let is_loopback = ipv4.as_bytes()[0] == 127;
                                let is_local = crate::net::net_core::current_netns()
                                    .device_list
                                    .lock()
                                    .values()
                                    .any(|iface| {
                                        iface.ip_addrs().iter().any(|c| c.address() == dst_ip)
                                    });
                                if is_loopback || is_local {
                                    send_to_lo = true;
                                    send_to_eth = false;
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    send_to_eth = true;
                }

                if send_to_lo {
                    log::debug!("[RoutingTxToken] send to lo");
                    lo_tx.consume(len, |b| {
                        b.copy_from_slice(&buf);
                    });
                }
                if send_to_eth {
                    log::debug!("[RoutingTxToken] send to eth");
                    eth_tx.consume(len, |b| {
                        b.copy_from_slice(&buf);
                    });
                }

                res
            }
        }
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
        self.inner.transmit(&buf);
        result
    }
}
