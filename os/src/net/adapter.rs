use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use riscv::addr::Address;
use spin::Mutex;
use smoltcp::phy::{Device, DeviceCapabilities, Loopback, Medium, RxToken, TxToken};
use smoltcp::wire::{ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress, Ipv4Address, Ipv4Packet};
use smoltcp::time::Instant;
use crate::drivers::NET_DEVICE;
use crate::drivers::net::NetDevice;

pub struct RoutingDevice {
    pub eth: SmoltcpDeviceAdapter,
    pub lo: Loopback,
}

impl RoutingDevice {
    pub fn new(eth: SmoltcpDeviceAdapter, lo: Loopback) -> Self {
        Self{eth,
        lo,
        }
    }
    
}

impl Device for RoutingDevice {
    type RxToken<'a> = RoutingRxToken<'a>;
    type TxToken<'a> = RoutingTxToken<'a>;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = self.eth.capabilities();
        caps.medium = Medium::Ethernet;
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
        })
    }
}

pub enum RoutingRxToken<'a> {
    Eth(<SmoltcpDeviceAdapter as Device>::RxToken<'a>),
    Lo(<Loopback as Device>::RxToken<'a>),
}

impl<'a> RxToken for RoutingRxToken<'a>  {
    fn consume<R, F>(self, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R {
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
    },
}

impl<'a> TxToken for RoutingTxToken<'a>  {
    fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R {
        match self {
            Self::Eth(t) => t.consume(len, f),
            Self::Lo(t) => t.consume(len, f),
            Self::Mixed { eth_tx, lo_tx } => {
                let mut buf = vec![0u8;len];
                let res = f(&mut buf);

                let mut send_to_lo = false;

                // Routing
                if let Ok(frame) = EthernetFrame::new_checked(&buf) {
                    let dst_mac = frame.dst_addr();
                    let is_broadcast = dst_mac.is_broadcast();
                    
                    let hw_addr = EthernetAddress(NET_DEVICE.mac_address());
                    let is_loopback_mac = dst_mac == hw_addr; 

                    // 判断包的目标 IP 是否是 127.x.x.x 环回段
                    let is_loopback_ip = match frame.ethertype() {
                    EthernetProtocol::Ipv4 => {
                        if let Ok(ipv4) = Ipv4Packet::new_checked(frame.payload()) {
                            ipv4.dst_addr().as_bytes()[0] == 127
                        } else {
                            false
                        }
                    }
                    EthernetProtocol::Arp => {
                        if let Ok(arp) = ArpPacket::new_checked(frame.payload()) {
                         // 检查 ARP 寻找的目标 IP (Target Protocol Address)
                        let target_ip = Ipv4Address::from_bytes(arp.target_protocol_addr());
                        arp.target_protocol_addr()[0] == 127
                        } else {
                            false
                        }
                    }
                    _ => false,
                    };

                    if is_loopback_ip || is_loopback_mac {
                        send_to_lo = true;
                    }
                }
                if send_to_lo {
                    log::info!("[RoutingTxToken] is loop");
                    lo_tx.consume(len, |b| {
                        b.copy_from_slice(&buf);
                    });
                } else {
                    log::info!("[RoutingTxToken] is not loop");
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
    pub fn new(inner: Arc<dyn NetDevice>) ->Self {
        Self{inner,
        }
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
        let mut buf = [0u8;2048];

        if let Some(len) = self.inner.receive(&mut buf) {
            let packet = buf[..len].to_vec();
            let rx = NetRxToken{buf:packet};
            let tx = NetTxToken{
                inner:self.inner.clone(),
            };
            Some((rx,tx))
        }else{
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
            F: FnOnce(&mut [u8]) -> R {
        f(&mut self.buf)
    }    
}

pub struct NetTxToken {
    inner: Arc<dyn NetDevice>,
}

impl TxToken for NetTxToken  {
    fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R {
        // 防空包
        if len == 0 {
            log::warn!("[NetTxToken] Attempted to send a zero-length packet, intercepting.");
            let mut empty_buf = [];
            return f(&mut empty_buf);
        }

        // 防止过大的包导致内存耗尽 (OOM)
        if len > 2048 {
            log::error!("[NetTxToken] Packet too large: {}, dropping.", len);
            let mut dummy_buf = vec![0u8; len];
            return f(&mut dummy_buf); 
        }

        let mut buf = vec![0u8;len];
        let result = f(&mut buf);        
        self.inner.transmit(&buf);
        result
    }
}