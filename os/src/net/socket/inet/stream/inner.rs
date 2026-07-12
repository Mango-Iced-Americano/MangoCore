//! TCP 内部状态机 —— 6 状态变体（Init / Connecting / Listening / Established / SelfConnected / Closed）
//!
//! 设计思路：将 TCP socket 生命周期划分为 6 种明确的变体，每种变体封装自己的数据（smoltcp handle、
//! 缓存 endpoint、连接结果等）。`Inner` 枚举统一管理，通过 match 分发操作。
//! 此架构对标 DragonOS `net/socket/inet/stream/inner.rs`。

use crate::net::routing::RouteSocketHandle;
use crate::net::{
    config::{lookup_source_ip, NET_INTERFACE},
    routing::InetProtocol,
    TCP_SOCKETS_TO_REMOVE,
};
use crate::trace_event;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use smoltcp::{
    socket::tcp::{self, SocketBuffer},
    wire::{IpAddress, IpEndpoint, IpListenEndpoint, IpVersion},
};

use crate::net::socket::inet::stream::inner::ConnectResult::{Connected, Refused, RefusedConsumed};
use crate::utils::error::{GeneralRet, SyscallErr};
use spin::Mutex;

// ── TCP Socket 大小常量 ──────────────────────────────────────────────

pub const DEFAULT_RX_BUF_SIZE: usize = 64 * 1024;
pub const DEFAULT_TX_BUF_SIZE: usize = 64 * 1024;
pub const TCP_MSS_DEFAULT: u32 = 1 << 15;
/// TCP maximum segment size
pub const TCP_MSS: u32 = if TCP_MSS_DEFAULT > 65536 {
    65536
} else {
    TCP_MSS_DEFAULT
};
pub const BACKLOG_SIZE: u32 = 16;
pub const LISTEN_BUFFER_SIZE: usize = 32 * 1024;

// EPollEvent 已移至 fs/vfs/event.rs，全内核统一使用该定义。
pub use crate::fs::vfs::event::EPollEvent;

// ── 连接结果枚举 ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ConnectResult {
    Connected,
    #[default]
    Connecting,
    Refused,
    RefusedConsumed,
}

// ── Closed ───────────────────────────────────────────────────────────

/// 显式的"已关闭"状态。不再持有任何 smoltcp handle。
#[derive(Debug, Clone, Copy)]
pub struct Closed {
    ver: IpVersion,
}

impl Closed {
    #[inline]
    pub fn new(ver: IpVersion) -> Self {
        Self { ver }
    }

    #[inline]
    pub fn local_endpoint(&self) -> IpEndpoint {
        match self.ver {
            IpVersion::Ipv4 => {
                IpEndpoint::new(IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED), 0)
            }
            IpVersion::Ipv6 => {
                IpEndpoint::new(IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED), 0)
            }
        }
    }
}

// ── 帮助函数 ─────────────────────────────────────────────────────────

fn new_smoltcp_socket_with_size(rx_size: usize, tx_size: usize) -> tcp::Socket<'static> {
    let rx_buffer = SocketBuffer::new(vec![0; rx_size]);
    let tx_buffer = SocketBuffer::new(vec![0; tx_size]);
    tcp::Socket::new(rx_buffer, tx_buffer)
}

fn new_smoltcp_socket() -> tcp::Socket<'static> {
    new_smoltcp_socket_with_size(DEFAULT_RX_BUF_SIZE, DEFAULT_TX_BUF_SIZE)
}

fn new_listen_smoltcp_socket(
    local_endpoint: IpListenEndpoint,
) -> Result<tcp::Socket<'static>, crate::utils::error::SyscallErr> {
    let mut socket = new_smoltcp_socket();
    socket.listen(local_endpoint).map_err(|e| match e {
        tcp::ListenError::InvalidState => SyscallErr::EINVAL,
        tcp::ListenError::Unaddressable => SyscallErr::EADDRINUSE,
    })?;
    Ok(socket)
}

/// 从 smoltcp 的 RouteSocketHandle 读取 tcp::State 并转换为 TcpStateCode (u64)
/// 用于 trace_event
pub(crate) fn tcp_state_code(state: &tcp::State) -> u64 {
    use tcp::State::*;
    match state {
        Closed => 0,
        Listen => 1,
        SynSent => 2,
        SynReceived => 3,
        Established => 4,
        FinWait1 => 5,
        FinWait2 => 6,
        CloseWait => 7,
        Closing => 8,
        LastAck => 9,
        TimeWait => 10,
    }
}

/// 封装对单个 smoltcp handle 的可变访问
pub(crate) fn with_tcp_mut<R>(
    handle: RouteSocketHandle,
    f: impl FnOnce(&mut tcp::Socket) -> R,
) -> Option<R> {
    NET_INTERFACE.tcp_routed_socket(handle, f)
}

pub(crate) fn with_tcp<R>(
    handle: RouteSocketHandle,
    f: impl FnOnce(&tcp::Socket) -> R,
) -> Option<R> {
    NET_INTERFACE.tcp_routed_socket(handle, |s| f(s))
}

// ── Init ─────────────────────────────────────────────────────────────

/// Init 状态：socket 刚创建，尚未 connect / listen。
/// - `Unbound`：尚未加入 SocketSet（分配了 smoltcp socket 但未 add_socket）
/// - `Bound`：已加入 SocketSet，有确定的本地 endpoint
#[derive(Debug)]
pub enum Init {
    Unbound(Box<tcp::Socket<'static>>, IpVersion),
    Bound {
        socket: Box<tcp::Socket<'static>>,
        local: IpEndpoint,
        pending_error: Option<SyscallErr>,
    },
}

impl Init {
    pub fn new(ver: IpVersion) -> Self {
        Init::Unbound(Box::new(new_smoltcp_socket()), ver)
    }

    /// 获取本地 endpoint（Bound 时返回真实地址，Unbound 时返回全零）
    pub fn local_endpoint(&self) -> IpEndpoint {
        match self {
            Init::Unbound(_, ver) => match ver {
                IpVersion::Ipv4 => {
                    IpEndpoint::new(IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED), 0)
                }
                IpVersion::Ipv6 => {
                    IpEndpoint::new(IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED), 0)
                }
            },
            Init::Bound { local, .. } => *local,
        }
    }

    /// 调整缓冲区大小（仅 Unbound 时可调；Bound 后直接调整 smoltcp socket）
    pub fn resize_buffers(&mut self, rx_size: usize, tx_size: usize) -> Result<(), SyscallErr> {
        match self {
            Init::Unbound(socket, _) => {
                let mut new_sock = new_smoltcp_socket_with_size(rx_size, tx_size);
                // 复制选项
                new_sock.set_nagle_enabled(socket.nagle_enabled());
                new_sock.set_ack_delay(socket.ack_delay());
                new_sock.set_keep_alive(socket.keep_alive());
                new_sock.set_timeout(socket.timeout());
                new_sock.set_hop_limit(socket.hop_limit());
                **socket = new_sock;
                Ok(())
            }
            Init::Bound { .. } => {
                // 此 smoltcp 版本不支持动态调整缓冲区大小
                log::warn!("[TCP] resize_buffers on Bound socket not supported");
                Ok(())
            }
        }
    }

    /// 将本 socket 加入 SocketSet，返回 Bound 状态。
    /// 如果已有 handle 则直接返回 Ok。
    pub fn bind_to_smoltcp(self) -> Result<(RouteSocketHandle, IpEndpoint), (Self, SyscallErr)> {
        match self {
            Init::Unbound(socket, ver) => {
                let socket = *socket;
                let local_ep = socket.local_endpoint().unwrap_or_else(|| {
                    let port =
                        crate::net::socket::inet::common::PortManager::alloc_ephemeral_port();
                    IpEndpoint::new(
                        match ver {
                            IpVersion::Ipv4 => {
                                IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED)
                            }
                            IpVersion::Ipv6 => {
                                IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED)
                            }
                        },
                        port,
                    )
                });
                let handle = NET_INTERFACE
                    .add_routed_socket(InetProtocol::Tcp, socket)
                    .ok_or_else(|| {
                        (
                            Init::Unbound(Box::new(new_smoltcp_socket()), ver),
                            SyscallErr::EAGAIN,
                        )
                    })?;
                Ok((handle, local_ep))
            }
            Init::Bound { local, .. } => {
                // Lazy bind: socket not yet in SocketSet; caller must attach before use
                Err((
                    Init::Unbound(Box::new(new_smoltcp_socket()), IpVersion::Ipv4),
                    SyscallErr::EINVAL,
                ))
            }
        }
    }
}

// ── Connecting ───────────────────────────────────────────────────────

/// 正在建立 TCP 连接（SYN_SENT / SYN_RCVD）。
/// 通过 `result` 字段跟踪握手进度。
#[derive(Debug)]
pub struct Connecting {
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub remote: IpEndpoint,
    pub result: Mutex<ConnectResult>,
    was_established: AtomicBool,
}

impl Connecting {
    pub fn new(handle: RouteSocketHandle, local: IpEndpoint, remote: IpEndpoint) -> Self {
        Self {
            handle,
            local,
            remote,
            result: Mutex::new(ConnectResult::Connecting),
            was_established: AtomicBool::new(false),
        }
    }

    pub fn local_endpoint(&self) -> IpEndpoint {
        self.local
    }

    pub fn remote_endpoint(&self) -> IpEndpoint {
        self.remote
    }

    /// 握手完成时调用，消耗 Connecting，返回对应状态
    pub fn into_result(self) -> (super::Inner, Result<(), SyscallErr>) {
        let result = *self.result.lock();
        let result_code: u64 = match result {
            ConnectResult::Connected => 1,
            ConnectResult::Connecting => 0,
            ConnectResult::Refused | ConnectResult::RefusedConsumed => 2,
        };
        trace_event!(0xB032, self.handle.0 as u64, result_code, 0, 0, 0, 0);
        match result {
            ConnectResult::Connected => {
                log::info!(
                    "[Connecting::into_result] handle {} -> Established",
                    self.handle
                );
                (
                    super::Inner::Established(Established::new(
                        self.handle,
                        self.local,
                        self.remote,
                    )),
                    Ok(()),
                )
            }
            ConnectResult::Connecting => (super::Inner::Connecting(self), Err(SyscallErr::EAGAIN)),
            ConnectResult::Refused | ConnectResult::RefusedConsumed => {
                log::info!(
                    "[Connecting::into_result] push handle {} to TCP_SOCKETS_TO_REMOVE (refused)",
                    self.handle
                );
                TCP_SOCKETS_TO_REMOVE.lock().push(self.handle);
                let ver = match self.local.addr {
                    IpAddress::Ipv4(_) => IpVersion::Ipv4,
                    IpAddress::Ipv6(_) => IpVersion::Ipv6,
                };
                (
                    super::Inner::Init(Init::new(ver)),
                    Err(SyscallErr::ECONNREFUSED),
                )
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(*self.result.lock(), ConnectResult::Connected)
    }

    pub fn failure_reason(&self) -> Option<SyscallErr> {
        if matches!(*self.result.lock(), ConnectResult::Refused) {
            Some(SyscallErr::ECONNREFUSED)
        } else {
            None
        }
    }

    pub fn consume_error(&self) {
        let mut guard = self.result.lock();
        if matches!(*guard, ConnectResult::Refused) {
            *guard = ConnectResult::RefusedConsumed;
        }
    }

    /// 查询 smoltcp 的握手状态，同步更新 `result` 和 pollee
    ///
    /// 只有当 was_established 为 true（即曾经进入过 Established 或 CloseWait 状态）
    /// 才认为连接成功建立。收到 RST 直接进入 Closed 但 was_established 为 false，
    /// 此时判定为连接被拒绝（ECONNREFUSED）。
    pub fn update_io_events(&self, pollee: &AtomicUsize) -> bool {
        let ready = with_tcp_mut(self.handle, |socket| {
            let state = socket.state();

            // 记录“是否曾经进入过 Established/CloseWait”
            if matches!(state, tcp::State::Established | tcp::State::CloseWait) {
                self.was_established.store(true, Ordering::Relaxed);
            }
            let was_established = self.was_established.load(Ordering::Relaxed);

            // ── 根据真实状态计算“理想结果” ──
            let ideal = if matches!(state, tcp::State::Established | tcp::State::CloseWait) {
                ConnectResult::Connected
            } else if socket.is_open() {
                // State nuance: `is_open()` 在 `SynSent`/`FinWait` 等中间态返回 `true`，
                // 但这些阶段连接尚未完成，仍应视为 `Connecting`。
                ConnectResult::Connecting
            } else if was_established {
                // 曾经建立过，后来关闭（非拒绝）
                ConnectResult::Connected
            } else {
                ConnectResult::Refused
            };

            // ── 更新 result（唯一例外：不把 RefusedConsumed 降级为 Refused） ──
            let mut result = self.result.lock();
            let old = *result;
            if matches!(old, ConnectResult::RefusedConsumed) && ideal == ConnectResult::Refused {
                // 已经消费过错误，保持 RefusedConsumed，不要再触发新的错误事件
            } else {
                *result = ideal;
            }

            // ── 根据新的 result 设置 pollee ──
            let events = match *result {
                ConnectResult::Connected => {
                    EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM // 可写，无 IN/ERR/HUP
                }
                ConnectResult::Connecting => {
                    EPollEvent::empty() // 握手期间所有事件清零
                }
                ConnectResult::Refused => {
                    EPollEvent::EPOLLIN
                        | EPollEvent::EPOLLRDNORM
                        | EPollEvent::EPOLLOUT
                        | EPollEvent::EPOLLWRNORM
                        | EPollEvent::EPOLLHUP
                        | EPollEvent::EPOLLRDHUP
                        | EPollEvent::EPOLLERR
                }
                ConnectResult::RefusedConsumed => {
                    EPollEvent::EPOLLIN
                        | EPollEvent::EPOLLRDNORM
                        | EPollEvent::EPOLLOUT
                        | EPollEvent::EPOLLWRNORM
                        | EPollEvent::EPOLLHUP
                        | EPollEvent::EPOLLRDHUP
                    // 没有 EPOLLERR，因为错误已经被消费过
                }
            };

            // 原子地替换整个事件集（简单可靠，不依赖旧的位）
            pollee.store(events.bits(), Ordering::Relaxed);

            // 返回 true 表示当前已经终结（可唤醒等待者）
            matches!(*result, Connected | Refused | RefusedConsumed)
        });
        ready.unwrap_or(false)
    }
}

// ── Listening ────────────────────────────────────────────────────────

/// 监听状态：包含多个 smoltcp listen socket 以实现 backlog 语义。
#[derive(Debug)]
pub struct Listening {
    pub handles: Vec<RouteSocketHandle>,
    connect: AtomicUsize,
    listen_addr: IpListenEndpoint,
}

impl Listening {
    pub fn new(handles: Vec<RouteSocketHandle>, listen_addr: IpListenEndpoint) -> Self {
        // Trace: record all listen handles and port
        for (i, h) in handles.iter().enumerate() {
            trace_event!(
                0xB034,
                h.0 as u64,
                i as u64,
                listen_addr.port as u64,
                0,
                0,
                0
            );
        }
        Self {
            handles,
            connect: AtomicUsize::new(0),
            listen_addr,
        }
    }

    pub fn local_endpoint(&self) -> IpEndpoint {
        IpEndpoint::new(
            self.listen_addr
                .addr
                .unwrap_or(IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED)),
            self.listen_addr.port,
        )
    }

    pub fn listen_addr(&self) -> IpListenEndpoint {
        self.listen_addr
    }

    pub fn accept(&mut self) -> Result<(RouteSocketHandle, IpEndpoint), SyscallErr> {
        // 遍历所有 handles 找到第一个已建立连接的 socket，
        // 不依赖 self.connect（epoll/pselect 事件提示用）避免事件同步延迟导致 accept 失败。
        log::info!(
            "[TCP::accept] checking {} listen sockets for new connections",
            self.handles.len()
        );
        let connected_idx = self
            .handles
            .iter()
            .position(|&h| {
                with_tcp_mut(h, |socket| {
                    socket.state() == smoltcp::socket::tcp::State::Established
                        || socket.state() == smoltcp::socket::tcp::State::CloseWait
                })
                .unwrap_or(false)
            })
            .ok_or(SyscallErr::EAGAIN)?;
        log::info!(
            "[TCP::accept] found established connection on listen socket handle {}, retrieving remote endpoint",
            self.handles[connected_idx]
        );
        // Look up the ifindex of the accepted handle BEFORE taking the mutable
        // borrow, so the replacement listen socket goes on the same interface stack.
        let accepted_handle = self.handles[connected_idx];
        let binding_ifindex = NET_INTERFACE
            .inner_handler(|inner_ref| inner_ref.bindings.get(&accepted_handle).map(|b| b.ifindex))
            .flatten()
            .unwrap_or(1);

        let connected = &mut self.handles[connected_idx];
        let remote_endpoint = with_tcp_mut(*connected, |socket| {
            socket
                .remote_endpoint()
                .expect("a connected TCP socket with no remote endpoint")
        })
        .expect("with_tcp_mut returned None for active socket");

        // 用新的 listen socket 替换已连接的 socket
        let new_listen =
            new_listen_smoltcp_socket(self.listen_addr).map_err(|_| SyscallErr::EADDRINUSE)?;

        let connected_handle = if let Some(mut new_handle) =
            NET_INTERFACE.add_routed_socket_on(InetProtocol::Tcp, new_listen, binding_ifindex)
        {
            core::mem::swap(connected, &mut new_handle);
            new_handle
        } else {
            // add_socket 失败（极少情况），直接把当前连接 handle 返回
            let h = *connected;
            h
        };

        // 重置 self.connect，下次 update_io_events 会重新计算
        self.connect.store(0, Ordering::Relaxed);

        Ok((connected_handle, remote_endpoint))
    }

    /// Check if any backlog handle has an established/active connection.
    /// Returns true when accept() would succeed without EAGAIN.
    pub fn has_pending_connection(&self) -> bool {
        self.handles.iter().any(|&h| {
            with_tcp(h, |socket| {
                socket.state() == smoltcp::socket::tcp::State::Established
                    || socket.state() == smoltcp::socket::tcp::State::CloseWait
            })
            .unwrap_or(false)
        })
    }

    pub fn update_io_events(&self, pollee: &AtomicUsize) {
        let position = self
            .handles
            .iter()
            .position(|&h| with_tcp_mut(h, |socket| socket.is_active()).unwrap_or(false));
        if let Some(position) = position {
            self.connect.store(position, Ordering::Relaxed);
            pollee.fetch_or(
                (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                Ordering::Relaxed,
            );
        } else {
            pollee.fetch_and(
                !(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                Ordering::Relaxed,
            );
        }
    }

    pub fn close(&self) {
        log::info!(
            "[Listening::close] closing {} listen sockets",
            self.handles.len()
        );
        for &h in &self.handles {
            with_tcp_mut(h, |socket| {
                if socket.is_active() {
                    log::info!(
                        "[Listening::close] aborting pending handle {} (state={:?})",
                        h,
                        socket.state()
                    );
                    socket.abort();
                } else {
                    socket.close();
                }
            });
            log::info!(
                "[Listening::close] push handle {} to TCP_SOCKETS_TO_REMOVE",
                h
            );
            TCP_SOCKETS_TO_REMOVE.lock().push(h);
        }
    }
}

// ── Established ──────────────────────────────────────────────────────

/// 已建立 TCP 连接（Established / CloseWait 等活跃状态）。
#[derive(Debug)]
pub struct Established {
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub peer: IpEndpoint,
}

impl Established {
    pub fn new(handle: RouteSocketHandle, local: IpEndpoint, peer: IpEndpoint) -> Self {
        Self {
            handle,
            local,
            peer,
        }
    }

    pub fn local_endpoint(&self) -> IpEndpoint {
        with_tcp_mut(self.handle, |socket| {
            socket.local_endpoint().unwrap_or(self.local)
        })
        .unwrap_or(self.local)
    }

    pub fn remote_endpoint(&self) -> IpEndpoint {
        with_tcp_mut(self.handle, |socket| {
            socket.remote_endpoint().unwrap_or(self.peer)
        })
        .unwrap_or(self.peer)
    }

    pub fn send_slice(&self, buf: &[u8]) -> Result<usize, SyscallErr> {
        with_tcp_mut(self.handle, |socket| {
            if socket.can_send() {
                socket.send_slice(buf).map_err(|_| SyscallErr::ECONNABORTED)
            } else {
                match socket.state() {
                    tcp::State::Closed => Err(SyscallErr::ECONNRESET),
                    tcp::State::TimeWait | tcp::State::Closing | tcp::State::LastAck => {
                        Err(SyscallErr::EPIPE)
                    }
                    _ => Err(SyscallErr::EAGAIN),
                }
            }
        })
        .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    /// 更新 pollee 缓存的 IO 事件
    pub fn update_io_events(&self, pollee: &AtomicUsize) {
        let _ = with_tcp_mut(self.handle, |socket| {
            let state = socket.state();

            let is_connected = matches!(state, tcp::State::Established | tcp::State::SynReceived);
            let fin_received = matches!(
                state,
                tcp::State::CloseWait
                    | tcp::State::LastAck
                    | tcp::State::Closing
                    | tcp::State::TimeWait
                    | tcp::State::Closed
            );
            let is_closed = matches!(state, tcp::State::TimeWait | tcp::State::Closed);

            // EPOLLOUT
            if socket.can_send() {
                pollee.fetch_or(
                    (EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM).bits(),
                    Ordering::Relaxed,
                );
            } else {
                pollee.fetch_and(
                    !(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM).bits(),
                    Ordering::Relaxed,
                );
            }

            // EPOLLIN
            if socket.can_recv() || fin_received {
                pollee.fetch_or(
                    (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                    Ordering::Relaxed,
                );
            } else {
                pollee.fetch_and(
                    !(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                    Ordering::Relaxed,
                );
            }

            // EPOLLHUP / EPOLLRDHUP / EPOLLERR
            if is_connected {
                pollee.fetch_and(
                    !(EPollEvent::EPOLLHUP | EPollEvent::EPOLLRDHUP | EPollEvent::EPOLLERR).bits(),
                    Ordering::Relaxed,
                );
            } else if fin_received && !is_closed {
                pollee.fetch_or(EPollEvent::EPOLLRDHUP.bits(), Ordering::Relaxed);
                pollee.fetch_and(
                    !(EPollEvent::EPOLLHUP | EPollEvent::EPOLLERR).bits(),
                    Ordering::Relaxed,
                );
            } else if is_closed {
                pollee.fetch_or(
                    (EPollEvent::EPOLLHUP | EPollEvent::EPOLLRDHUP).bits(),
                    Ordering::Relaxed,
                );
                pollee.fetch_and(!EPollEvent::EPOLLERR.bits(), Ordering::Relaxed);
            }
        });
    }

    pub fn close(&self) {
        log::info!("[Established::close] closing handle {}", self.handle);
        let _ = with_tcp_mut(self.handle, |socket| socket.close());
        log::info!(
            "[Established::close] push handle {} to TCP_SOCKETS_TO_REMOVE",
            self.handle
        );
        TCP_SOCKETS_TO_REMOVE.lock().push(self.handle);
    }
}

// ── SelfConnected ────────────────────────────────────────────────────

/// Linux 兼容的 TCP "自连接"（connect 到自身相同的 addr:port）。
/// 内部使用 VecDeque 队列模拟回环收发。smoltcp handle 保留但实际数据不走网络栈。
#[derive(Debug)]
pub struct SelfConnected {
    pub handle: RouteSocketHandle,
    pub local: IpEndpoint,
    pub buf: Mutex<VecDeque<u8>>,
    pub rx_cap: AtomicUsize,
    pub send_shutdown: AtomicBool,
}

impl SelfConnected {
    pub fn new(handle: RouteSocketHandle, local: IpEndpoint, rx_cap: usize) -> Self {
        Self {
            handle,
            local,
            buf: Mutex::new(VecDeque::new()),
            rx_cap: AtomicUsize::new(rx_cap),
            send_shutdown: AtomicBool::new(false),
        }
    }

    pub fn local_endpoint(&self) -> IpEndpoint {
        self.local
    }

    pub fn remote_endpoint(&self) -> IpEndpoint {
        self.local
    }

    pub fn send_slice(&self, data: &[u8]) -> Result<usize, SyscallErr> {
        if self.send_shutdown.load(Ordering::Acquire) {
            return Err(SyscallErr::EPIPE);
        }
        if data.is_empty() {
            return Ok(0);
        }
        let cap = self.rx_cap.load(Ordering::Relaxed);
        let mut q = self.buf.lock();
        let free = cap.saturating_sub(q.len());
        if free == 0 {
            return Err(SyscallErr::EAGAIN);
        }
        let n = core::cmp::min(free, data.len());
        q.extend(&data[..n]);
        Ok(n)
    }

    pub fn recv_into(&self, out: &mut [u8], peek: bool) -> Result<usize, SyscallErr> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut q = self.buf.lock();
        if q.is_empty() {
            if self.send_shutdown.load(Ordering::Acquire) {
                return Ok(0); // EOF
            }
            return Err(SyscallErr::EAGAIN);
        }
        let n = core::cmp::min(out.len(), q.len());
        for (i, b) in q.iter().take(n).enumerate() {
            out[i] = *b;
        }
        if !peek {
            for _ in 0..n {
                q.pop_front();
            }
        }
        Ok(n)
    }

    pub fn set_send_shutdown(&self) {
        self.send_shutdown.store(true, Ordering::Release);
    }

    pub fn update_io_events(&self, pollee: &AtomicUsize) {
        let send_shutdown = self.send_shutdown.load(Ordering::Acquire);
        let queued = self.buf.lock().len();
        let cap = self.rx_cap.load(Ordering::Relaxed);
        let writable = !send_shutdown && queued < cap;
        let readable = queued > 0 || send_shutdown;

        if writable {
            pollee.fetch_or(
                (EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM).bits(),
                Ordering::Relaxed,
            );
        } else {
            pollee.fetch_and(
                !(EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM).bits(),
                Ordering::Relaxed,
            );
        }
        if readable {
            pollee.fetch_or(
                (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                Ordering::Relaxed,
            );
        } else {
            pollee.fetch_and(
                !(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
                Ordering::Relaxed,
            );
        }
    }

    pub fn close(&self) {
        log::info!(
            "[SelfConnected::close] push handle {} to TCP_SOCKETS_TO_REMOVE",
            self.handle
        );
        TCP_SOCKETS_TO_REMOVE.lock().push(self.handle);
    }
}

// ── Inner 枚举 ───────────────────────────────────────────────────────

/// TCP 状态机枚举 —— 6 种明确变体
#[derive(Debug)]
pub enum Inner {
    Init(Init),
    Connecting(Connecting),
    Listening(Listening),
    Established(Established),
    SelfConnected(SelfConnected),
    Closed(Closed),
}

impl Inner {
    /// 获取本地 endpoint（各变体均返回有意义的值）
    pub fn local_endpoint(&self) -> IpEndpoint {
        match self {
            Inner::Init(init) => init.local_endpoint(),
            Inner::Connecting(c) => c.local_endpoint(),
            Inner::Listening(l) => l.local_endpoint(),
            Inner::Established(e) => e.local_endpoint(),
            Inner::SelfConnected(s) => s.local_endpoint(),
            Inner::Closed(c) => c.local_endpoint(),
        }
    }

    /// 获取远端 endpoint（仅 Established / SelfConnected / Connecting 有意义）
    pub fn remote_endpoint(&self) -> Option<IpEndpoint> {
        match self {
            Inner::Init(_) | Inner::Listening(_) | Inner::Closed(_) => None,
            Inner::Connecting(c) => Some(c.remote_endpoint()),
            Inner::Established(e) => Some(e.remote_endpoint()),
            Inner::SelfConnected(s) => Some(s.remote_endpoint()),
        }
    }

    pub fn send_buffer_size(&self) -> usize {
        match self {
            Inner::Closed(_) => 0,
            Inner::SelfConnected(sc) => sc.rx_cap.load(Ordering::Relaxed),
            Inner::Init(Init::Bound { socket, .. }) => socket.send_capacity(),
            Inner::Init(Init::Unbound(s, _)) => s.send_capacity(),
            Inner::Connecting(c) => with_tcp_mut(c.handle, |s| s.send_capacity()).unwrap_or(0),
            Inner::Listening(_) => 0,
            Inner::Established(e) => with_tcp_mut(e.handle, |s| s.send_capacity()).unwrap_or(0),
        }
    }

    pub fn recv_buffer_size(&self) -> usize {
        match self {
            Inner::Closed(_) => 0,
            Inner::SelfConnected(sc) => sc.rx_cap.load(Ordering::Relaxed),
            Inner::Init(Init::Bound { socket, .. }) => socket.recv_capacity(),
            Inner::Init(Init::Unbound(s, _)) => s.recv_capacity(),
            Inner::Connecting(c) => with_tcp_mut(c.handle, |s| s.recv_capacity()).unwrap_or(0),
            Inner::Listening(_) => 0,
            Inner::Established(e) => with_tcp_mut(e.handle, |s| s.recv_capacity()).unwrap_or(0),
        }
    }

    pub fn close(&self) {
        match self {
            Inner::Init(init) => match init {
                Init::Unbound(_, _) => {
                    log::info!("[Inner::close] Init::Unbound — no handle to close");
                }
                Init::Bound { .. } => {
                    log::info!(
                        "[Inner::close] Init::Bound — dropping boxed socket (not yet attached)"
                    );
                }
            },
            Inner::Connecting(c) => {
                log::info!("[Inner::close] Connecting — closing handle {}", c.handle);
                with_tcp_mut(c.handle, |socket| socket.close());
                log::info!(
                    "[Inner::close] push handle {} to TCP_SOCKETS_TO_REMOVE",
                    c.handle
                );
                TCP_SOCKETS_TO_REMOVE.lock().push(c.handle);
            }
            Inner::Listening(l) => {
                log::info!("[Inner::close] Listening — delegating to Listening::close()");
                l.close();
            }
            Inner::Established(e) => {
                log::info!("[Inner::close] Established — delegating to Established::close()");
                e.close();
            }
            Inner::SelfConnected(s) => {
                log::info!("[Inner::close] SelfConnected — delegating to SelfConnected::close()");
                s.close();
            }
            Inner::Closed(_) => {
                log::info!("[Inner::close] Closed — already closed, nothing to do");
            }
        }
    }

    /// 转换为 Linux TCP 状态码（用于 getsockopt TCP_INFO）
    pub fn tcp_state_code(&self) -> u8 {
        match self {
            Inner::Init(_) => 7,       // TCP_CLOSE
            Inner::Connecting(_) => 2, // TCP_SYN_SENT
            Inner::Listening(_) => 10, // TCP_LISTEN
            Inner::Established(e) => with_tcp_mut(e.handle, |socket| {
                let state = socket.state();
                match state {
                    tcp::State::Established => 1,
                    tcp::State::CloseWait => 8,
                    tcp::State::FinWait1 => 4,
                    tcp::State::FinWait2 => 5,
                    tcp::State::Closing => 11,
                    tcp::State::LastAck => 9,
                    _ => 7,
                }
            })
            .unwrap_or(7),
            Inner::SelfConnected(_) => 1, // TCP_ESTABLISHED
            Inner::Closed(_) => 7,        // TCP_CLOSE
        }
    }
}
