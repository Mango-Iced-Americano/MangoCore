use super::Mutex;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{RoutingDevice, SmoltcpDeviceAdapter};
use crate::net::udp::dispatch_udp_packets;
use crate::net::{GATEWAY, UDP_SOCKETS_TO_REMOVE};
use crate::net::{LOCAL_IP, TCP_SOCKETS_TO_REMOVE};
use crate::timer::current_time_duration;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use downcast_rs::Downcast;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium},
    socket::dhcpv4::{Event as Dhcpv4Event, Socket as Dhcpv4Socket},
    socket::{raw, tcp, udp, AnySocket},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};

pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub fn init() {
    // If NET_DEVICE exists: init NetInterface
    if NET_DEVICE.lock().is_some() {
        NET_INTERFACE.init();
        let device = NET_DEVICE.lock();
        let dev_ref = device.as_ref().unwrap();
        let mac = dev_ref.mac_address();
        println!("[kernel] nic init success!");
        println!(
            "[kernel] MAC : {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
    } else {
        println!("[kernel] nic init fail, network disabled");
    }
}

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}

pub struct NetInterfaceInner<'a> {
    pub device: RoutingDevice,
    // pub device: Loopback,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}

impl<'a> NetInterfaceInner<'a> {
    fn new() -> Self {
        // let mut device = Loopback::new(Medium::Ethernet);
        // NET_DEVICE is guaranteed Some at this point (init() checks before calling)
        let net_dev = NET_DEVICE.lock().as_ref().unwrap().clone();
        let eth_device = SmoltcpDeviceAdapter::new(net_dev);
        let lo_device = Loopback::new(Medium::Ethernet);
        let mut device = RoutingDevice::new(eth_device, lo_device);

        let mac = device.hw_addr.0;
        let hw_addr = HardwareAddress::Ethernet(EthernetAddress(mac));
        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let config = Config::new(hw_addr);
        let mut iface = Interface::new(config, &mut device, now);

        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
                .unwrap();
            addrs.push(IpCidr::new(LOCAL_IP, 24)).unwrap();
        });

        // 默认路由
        iface
            .routes_mut()
            .add_default_ipv4_route(match GATEWAY {
                IpAddress::Ipv4(v4) => v4,
                _ => unreachable!("GATEWAY is always IPv4"),
            })
            .unwrap();

        Self {
            device,
            iface,
            sockets: SocketSet::new(vec![]),
        }
    }
}

impl<'a> NetInterface<'a> {
    pub fn init(&self) {
        self._init();
    }

    pub fn add_socket<T>(&self, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        self._add_socket(socket)
    }

    pub fn _init(&self) {
        *self.inner.lock() = Some(NetInterfaceInner::new());
    }
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn _add_socket<T>(&self, socket: T) -> Option<SocketHandle>
    where
        T: AnySocket<'a>,
    {
        Some(self.inner.lock().as_mut()?.sockets.add(socket))
    }

    pub fn tcp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut tcp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<tcp::Socket>(handler);
        Some(f(socket))
    }

    pub fn udp_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut udp::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<udp::Socket>(handler);
        Some(f(socket))
    }

    pub fn raw_socket<T>(
        &self,
        handler: SocketHandle,
        f: impl FnOnce(&mut raw::Socket) -> T,
    ) -> Option<T> {
        let mut inner = self.inner.lock();
        let inner_ref = inner.as_mut()?;
        let socket = inner_ref.sockets.get_mut::<raw::Socket>(handler);
        Some(f(socket))
    }

    pub fn inner_handler<T>(&self, f: impl FnOnce(&mut NetInterfaceInner<'a>) -> T) -> Option<T> {
        Some(f(self.inner.lock().as_mut()?))
    }

    pub fn poll(&self) {
        if self.inner.lock().is_none() {
            return;
        }
        self._poll()
    }
    pub fn _poll(&self) {
        log::trace!("[NetInterface::poll] poll...");
        self.inner_handler(|inner| {
            {
                // 使用 drain(..) 一次性清空队列并取出所有元素
                let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
                for handle in to_remove.drain(..) {
                    inner.sockets.remove(handle);
                    log::info!(
                        "[NetInterface] Successfully removed underlying socket {}",
                        handle
                    );
                }
            }
            // poll 必须在删除 TCP socket 之前，这样 drop 时 close() 触发的
            // FIN/ACK 握手能在这个 poll 周期内完成（loopback 下一次 poll 即可完成）
            inner.iface.poll(
                Instant::from_millis(current_time_duration().as_millis() as i64),
                &mut inner.device,
                &mut inner.sockets,
            );
            {
                let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
                let ready: Vec<SocketHandle> = to_remove
                    .iter()
                    .filter(|&&h| {
                        let socket = inner.sockets.get::<tcp::Socket>(h);
                        socket.state() == tcp::State::Closed
                            || socket.state() == tcp::State::TimeWait
                    })
                    .copied()
                    .collect();
                for &h in &ready {
                    inner.sockets.remove(h);
                    log::info!(
                        "[NetInterface] Successfully removed underlying TCP socket {}",
                        h
                    );
                }
                to_remove.retain(|h| !ready.contains(h));
            }

            dispatch_udp_packets(inner);
        });
        // poll 结束后唤醒所有 TCP/RAW socket 的等待队列
        crate::net::wake_tcp_waiters();
        crate::net::wake_raw_waiters();
    }
    pub fn remove(&self, handler: SocketHandle) {
        self._remove(handler)
    }
    pub fn _remove(&self, handler: SocketHandle) {
        if let Some(inner) = self.inner.lock().as_mut() {
            inner.sockets.remove(handler);
        }
    }
}

pub fn lookup_source_ip(dest_ip: IpAddress) -> IpAddress {
    let is_loopback = match dest_ip {
        IpAddress::Ipv4(ipv4) => ipv4.0[0] == 127,
        _ => false, // 暂时忽略 IPv6
    };
    if is_loopback {
        // 如果目标是回环，网络层推荐使用回环地址
        IpAddress::v4(127, 0, 0, 1)
    } else {
        // 否则走默认路由，网络层推荐使用以太网网卡地址
        IpAddress::v4(10, 0, 2, 15)
    }
}
