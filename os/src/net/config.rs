use super::Mutex;
use crate::drivers::NET_DEVICE;
use crate::net::adapter::{RoutingDevice, SmoltcpDeviceAdapter};
use crate::net::socket::inet::datagram::udp::dispatch_udp_packets;
use crate::net::socket::inet::stream::inner::tcp_state_code;
use crate::net::{TCP_SOCKETS, TCP_SOCKETS_TO_REMOVE, UDP_SOCKETS_TO_REMOVE};
use crate::timer::current_time_duration;
use crate::trace_event;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    phy::{Device, Loopback, Medium},
    socket::{raw, tcp, udp, AnySocket},
    time::Instant,
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address},
};

pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub fn init() {
    if NET_DEVICE.lock().is_none() {
        println!("[kernel] net device unavailable, skipping net interface initialization");
        return;
    }
    NET_INTERFACE.init();
    println!("[kernel] net interface initialized (RoutingDevice: lo + eth)");
}

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}

pub struct NetInterfaceInner<'a> {
    pub device: RoutingDevice,
    // pub device: SmoltcpDeviceAdapter,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}

impl<'a> NetInterfaceInner<'a> {
    fn new() -> Self {
        let net_device = NET_DEVICE
            .lock()
            .take()
            .expect("NET_DEVICE not initialized before net::config::init()");
        let mut eth = SmoltcpDeviceAdapter::new(net_device);
        let lo = Loopback::new(Medium::Ip);
        // let lo = Loopback::new(Medium::Ethernet);
        let mut device = RoutingDevice::new(eth, lo);

        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let config = Config::new(HardwareAddress::Ethernet(EthernetAddress([
            0, 0, 0, 0, 0, 0,
        ])));
        let mut iface = Interface::new(config, &mut device, now);
        // let mut iface = Interface::new(config, &mut eth, now);
        // 双 IP: loopback + 物理网卡
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
                .unwrap();
            addrs
                .push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24))
                .unwrap();
        });

        // 默认路由: 0.0.0.0/0 via 10.0.2.2
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
            .unwrap();

        Self {
            device,
            // device: eth,
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
        self.poll_once();
    }

    /// Non-blocking poll: skip if the inner lock is already held
    /// (e.g., a syscall handler is already polling).
    /// Safe for use in interrupt contexts — never spins.
    pub fn try_poll(&self) -> bool {
        let guard = self.inner.try_lock();
        match guard {
            Some(inner) if inner.is_some() => {
                drop(inner);
                self.poll_once();
                true
            }
            _ => false, // lock held by another context, or NetInterface not yet initialized
        }
    }
    fn poll_once(&self) -> bool {
        let mut progressed = false;
        self.inner_handler(|inner| {
            // Trace: dump all TCP socket states BEFORE poll
            for (handle, sock) in inner.sockets.iter() {
                if let smoltcp::socket::Socket::Tcp(tcp_sock) = sock {
                    let sc = tcp_state_code(&tcp_sock.state());
                    trace_event!(0xB035, handle.as_usize() as u64, sc, 0, 0, 0, 0);
                }
            }

            // 1. 先清理标记删除的 UDP sockets
            let mut to_remove = UDP_SOCKETS_TO_REMOVE.lock();
            for handle in to_remove.drain(..) {
                inner.sockets.remove(handle);
            }
            drop(to_remove);

            // 2. 驱动协议栈
            let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
            progressed = inner
                .iface
                .poll(timestamp, &mut inner.device, &mut inner.sockets);

            // 3. 清理符合条件的 TCP sockets
            let mut to_remove = TCP_SOCKETS_TO_REMOVE.lock();
            let pending = to_remove.len();
            let ready: Vec<SocketHandle> = to_remove
                .iter()
                .filter(|&&h| {
                    let socket = inner.sockets.get::<tcp::Socket>(h);
                    let state = socket.state();
                    let can_remove =
                        state == tcp::State::Closed;
                    if !can_remove {
                        log::debug!(
                            "[NetInterface::poll_once] TCP handle {} not ready yet (state={:?}), deferring",
                            h, state
                        );
                    }
                    can_remove
                })
                .copied()
                .collect();
            if !ready.is_empty() {
                log::info!(
                    "[NetInterface::poll_once] removing {} of {} pending TCP sockets",
                    ready.len(),
                    pending
                );
            }
            for h in &ready {
                inner.sockets.remove(*h);
                log::info!("[NetInterface::poll_once] TCP socket {} fully removed from SocketSet", h);
            }
            to_remove.retain(|h| !ready.contains(h));
            if to_remove.len() > 0 {
                log::debug!("[NetInterface::poll_once] {} TCP handles still pending removal", to_remove.len());
            }
            drop(to_remove);

            // 4. 分发 UDP 包（必须在每次 poll 后立刻做）
            log::debug!("[poll_once] about to dispatch_udp_packets");
            dispatch_udp_packets(inner);

            // Trace: dump all TCP socket states AFTER poll
            // for (handle, sock) in inner.sockets.iter() {
            //     if let smoltcp::socket::Socket::Tcp(tcp_sock) = sock {
            //         let sc = tcp_state_code(&tcp_sock.state());
            //         trace_event!(0xB035, handle.as_usize() as u64, sc, 1, 0, 0, 0);
            //     }
            // }
        });

        // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        // crate::net::wake_tcp_waiters();
        // crate::net::wake_raw_waiters();

        // Trace: 记录 poll 后仍在连接中的 TCP socket 数
        // {
        //     let sockets = TCP_SOCKETS.lock();
        //     trace_event!(0xB033, sockets.len() as u64, 0, 0, 0, 0, 0);
        // }
        // config.rs poll_once() 中，在 poll 调用后加：
        // trace_event!(0xB036, progressed as u64, 0, 0, 0, 0, 0); // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        progressed
    }

    pub fn poll_until_quiescent(&self) {
        while self.try_poll() {
            // 继续推进，直到没有数据可处理
            crate::task::try_yield(); // 可选：避免占着 CPU 不放
        }
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
        // poll 结束后同步所有 TCP socket 的 IO 事件到 pollee（对标 DragonOS on_iface_events）
        {
            let sockets = crate::net::TCP_SOCKETS.lock();
            for weak in sockets.iter() {
                if let Some(socket) = weak.upgrade() {
                    socket.update_io_events();
                }
            }
        }
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
    // 环回目标走 127.0.0.1，其他走物理网卡 IP
    match dest_ip {
        IpAddress::Ipv4(addr) if addr.0[0] == 127 => IpAddress::v4(127, 0, 0, 1),
        _ => IpAddress::v4(10, 0, 2, 15),
    }
}
