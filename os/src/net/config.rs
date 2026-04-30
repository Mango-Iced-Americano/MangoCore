use super::Mutex;
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
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr},
};

pub static NET_INTERFACE: NetInterface = NetInterface::new();

pub fn init() {
    // Loopback-only mode: always initialize without physical NIC
    NET_INTERFACE.init();
    println!("[kernel] loopback-only mode (NIC disabled)");
}

pub struct NetInterface<'a> {
    inner: Mutex<Option<NetInterfaceInner<'a>>>,
}

pub struct NetInterfaceInner<'a> {
    // pub device: RoutingDevice,
    pub device: Loopback,
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
}

impl<'a> NetInterfaceInner<'a> {
    fn new() -> Self {
        let mut device = Loopback::new(Medium::Ethernet);

        let now = Instant::from_millis(current_time_duration().as_millis() as i64);
        let config = Config::new(HardwareAddress::Ethernet(EthernetAddress([
            0, 0, 0, 0, 0, 0,
        ])));
        let mut iface = Interface::new(config, &mut device, now);

        // Only 127.0.0.1/8 — no physical NIC, no default route
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8))
                .unwrap();
        });

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
        self.poll_once();
    }

    /// Non-blocking poll: skip if the inner lock is already held
    /// (e.g., a syscall handler is already polling).
    /// Safe for use in interrupt contexts — never spins.
    pub fn try_poll(&self) {
        let guard = self.inner.try_lock();
        match guard {
            Some(inner) if inner.is_some() => {
                drop(inner);
                self.poll_once();
            }
            _ => {} // lock held by another context, or NetInterface not yet initialized
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
                        state == tcp::State::Closed || state == tcp::State::TimeWait;
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
            dispatch_udp_packets(inner);

            // Trace: dump all TCP socket states AFTER poll
            for (handle, sock) in inner.sockets.iter() {
                if let smoltcp::socket::Socket::Tcp(tcp_sock) = sock {
                    let sc = tcp_state_code(&tcp_sock.state());
                    trace_event!(0xB035, handle.as_usize() as u64, sc, 1, 0, 0, 0);
                }
            }
        });

        // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        crate::net::wake_tcp_waiters();
        crate::net::wake_raw_waiters();

        // Trace: 记录 poll 后仍在连接中的 TCP socket 数
        {
            let sockets = TCP_SOCKETS.lock();
            trace_event!(0xB033, sockets.len() as u64, 0, 0, 0, 0, 0);
        }
        // config.rs poll_once() 中，在 poll 调用后加：
        trace_event!(0xB036, progressed as u64, 0, 0, 0, 0, 0); // 5. 更新所有 TCP/RAW socket 事件并唤醒等待者
        progressed
    }

    pub fn poll_until_quiescent(&self) {
        while self.poll_once() {
            // 继续推进，直到没有数据可处理
            crate::task::suspend_current_and_run_next(); // 可选：避免占着 CPU 不放
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

pub fn lookup_source_ip(_dest_ip: IpAddress) -> IpAddress {
    // Loopback-only mode: always return 127.0.0.1
    IpAddress::v4(127, 0, 0, 1)
}
