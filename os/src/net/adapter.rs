use core::convert::TryInto;
use core::result;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
// use riscv::addr::Address;
use crate::drivers::net::NetDevice;
use smoltcp::phy::{Device, DeviceCapabilities, Loopback, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress, Ipv4Address, Ipv4Packet,
};
use spin::Mutex;

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
                // let mut buf = vec![0u8; len];
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
                                let is_local = crate::net::net_core::IFACES.lock().iter()
                                    .any(|d| d.ip_addrs.iter().any(|c| c.address() == dst_ip));
                                if is_loopback || is_local {
                                    log::debug!("[RoutingTxToken] dst={} -> local delivery (lo)", dst_addr);
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
                                let ipv4 = Ipv4Address::from_bytes(&target_ip[..4].try_into().unwrap_or([0;4]));
                                let dst_ip = IpAddress::Ipv4(ipv4);
                                let is_loopback = ipv4.as_bytes()[0] == 127;
                                let is_local = crate::net::net_core::IFACES.lock().iter()
                                    .any(|d| d.ip_addrs.iter().any(|c| c.address() == dst_ip));
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
                    // if len > 1500 {
                    //     log::warn!(
                    //         "[Routing] Packet too large for eth ({}), dropping instead of crashing",
                    //         len
                    //     );
                    //     return res;
                    // }
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
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
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

        // // 防止过大的包导致内存耗尽 (OOM)
        // if len > 2048 {
        //     log::error!("[NetTxToken] Packet too large: {}, dropping.", len);
        //     let mut dummy_buf = vec![0u8; len];
        //     return f(&mut dummy_buf);
        // }

        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        self.inner.transmit(&buf);
        result
    }
}
