//! TCP Socket —— TcpSocket
//!
//! 架构对标 DragonOS `net/socket/inet/stream/mod.rs`。
//! 使用 6 状态 Inner 枚举管理 TCP 状态机：
//!   Init / Connecting / Listening / Established / SelfConnected / Closed
//!
//! TcpSocket 包装：
//!   - inner: Mutex<Inner>        — 状态机
//!   - pollee: AtomicUsize        — 缓存 EPOLL 事件
//!   - recv/send/connect/accept waiters — 等待队列

pub mod events;
pub mod inner;
pub mod io;
pub mod lifecycle;
pub mod tcp_info;

pub use inner::Closed;
pub use inner::{Inner, TCP_MSS};
pub use tcp_info::TcpInfo;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use smoltcp::socket::tcp;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, IpVersion};
use spin::Mutex;

use crate::net::routing::RouteSocketHandle;
use crate::net::syscall::common::MsgFlags;
use crate::net::{config::NET_INTERFACE, Endpoint, Socket, SocketFile, PSOCK};
use crate::{
    fs::vfs::{self, FileFlags},
    mm::UserBuffer,
    task::{current_task, WaitQueue},
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};

use self::inner::{
    with_tcp_mut, Connecting, Established, Init, Listening, SelfConnected, BACKLOG_SIZE,
};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::net::socket::inet::common::{BoundInner, PortManager};
use crate::net::socket::inet::stream::inner::ConnectResult;
use crate::trace_event;

#[cfg(feature = "net_perf_diag")]
const TCP_RECV_PERF_REPORT_INTERVAL_SECS: usize = 2;
#[cfg(feature = "net_perf_diag")]
const TCP_RECV_PERF_TIME_CHECK_MASK: usize = 0x1f;
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_EAGAIN: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_ZERO: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_REQUESTED: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "net_perf_diag")]
static TCP_RECV_PERF_LAST_REPORT: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "net_perf_diag")]
fn record_tcp_recv_perf(requested: usize, result: &Result<isize, SyscallErr>) {
    TCP_RECV_PERF_CALLS.fetch_add(1, Ordering::Relaxed);
    TCP_RECV_PERF_REQUESTED.fetch_add(requested, Ordering::Relaxed);
    match result {
        Ok(bytes) if *bytes > 0 => {
            TCP_RECV_PERF_BYTES.fetch_add(*bytes as usize, Ordering::Relaxed);
        }
        Ok(_) => {
            TCP_RECV_PERF_ZERO.fetch_add(1, Ordering::Relaxed);
        }
        Err(SyscallErr::EAGAIN) => {
            TCP_RECV_PERF_EAGAIN.fetch_add(1, Ordering::Relaxed);
        }
        Err(_) => {}
    }

    let calls = TCP_RECV_PERF_CALLS.load(Ordering::Relaxed);
    if calls & TCP_RECV_PERF_TIME_CHECK_MASK != 0 {
        return;
    }
    let now = crate::hal::get_time();
    let frequency = crate::hal::get_clock_freq().max(1);
    let previous = TCP_RECV_PERF_LAST_REPORT.load(Ordering::Relaxed);
    if previous == 0 {
        let _ = TCP_RECV_PERF_LAST_REPORT.compare_exchange(
            0,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        return;
    }
    let elapsed_ticks = now.wrapping_sub(previous);
    if elapsed_ticks < frequency.saturating_mul(TCP_RECV_PERF_REPORT_INTERVAL_SECS) {
        return;
    }
    if TCP_RECV_PERF_LAST_REPORT
        .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let elapsed_ms = elapsed_ticks.saturating_mul(1000) / frequency;
    let calls = TCP_RECV_PERF_CALLS.swap(0, Ordering::Relaxed);
    let bytes = TCP_RECV_PERF_BYTES.swap(0, Ordering::Relaxed);
    let eagain = TCP_RECV_PERF_EAGAIN.swap(0, Ordering::Relaxed);
    let zero = TCP_RECV_PERF_ZERO.swap(0, Ordering::Relaxed);
    let requested = TCP_RECV_PERF_REQUESTED.swap(0, Ordering::Relaxed);
    println!(
        "[net-perf][tcp-rx] dt_ms={} calls={} bytes={} kib_s={} avg_req={} eagain={} zero={}",
        elapsed_ms,
        calls,
        bytes,
        bytes.saturating_mul(1000) / elapsed_ms.max(1) / 1024,
        requested / calls.max(1),
        eagain,
        zero
    );
}

/// TCP Socket —— 对外表现为 Socket trait
pub struct TcpSocket {
    pub inner: Mutex<Inner>,
    pub pollee: AtomicUsize,
    pub read_shutdown: AtomicBool,
    pub write_shutdown: AtomicBool,
    pub reuse_addr: AtomicBool,
    multicast_group_joined: AtomicBool,
    pub bound: Mutex<BoundInner>,
    pub bound_ifindex: Mutex<Option<u32>>,
    pub recv_waiters: EventWaitQueue,
    pub send_waiters: EventWaitQueue,
    pub connect_waiters: EventWaitQueue,
    pub accept_waiters: EventWaitQueue,
    pub ip_version: IpVersion,
    ipv6_v6only: AtomicBool,
    fast_route_id: AtomicUsize,
    fast_ifindex: AtomicU32,
    fast_state: AtomicU8,
}

impl TcpSocket {
    pub fn new(ver: IpVersion) -> Self {
        Self {
            inner: Mutex::new(Inner::Init(Init::new(ver))),
            pollee: AtomicUsize::new(0),
            read_shutdown: AtomicBool::new(false),
            write_shutdown: AtomicBool::new(false),
            reuse_addr: AtomicBool::new(false),
            multicast_group_joined: AtomicBool::new(false),
            bound: Mutex::new(BoundInner::new()),
            bound_ifindex: Mutex::new(None),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            connect_waiters: EventWaitQueue::new(),
            accept_waiters: EventWaitQueue::new(),
            ip_version: ver,
            ipv6_v6only: AtomicBool::new(false),
            fast_route_id: AtomicUsize::new(0),
            fast_ifindex: AtomicU32::new(0),
            fast_state: AtomicU8::new(0),
        }
    }

    pub fn bound_inner(&self) -> BoundInner {
        self.bound.lock().clone()
    }

    fn addr_family_matches(&self, addr: IpAddress) -> bool {
        match self.ip_version {
            IpVersion::Ipv4 => matches!(addr, IpAddress::Ipv4(_)),
            IpVersion::Ipv6 => {
                if !self.ipv6_v6only.load(Ordering::Acquire) && matches!(addr, IpAddress::Ipv4(_)) {
                    return true;
                }
                matches!(addr, IpAddress::Ipv6(_))
            }
        }
    }

    fn normalize_ipv4_mapped(&self, addr: IpAddress) -> IpAddress {
        if let IpAddress::Ipv6(v6) = addr {
            if let Some(ipv4) = v6.as_ipv4() {
                return IpAddress::Ipv4(ipv4);
            }
        }
        addr
    }

    /// 注册到全局 TCP_SOCKETS 表
    pub fn register_tcp_socket(socket: &Arc<Self>) {
        crate::net::TCP_SOCKETS.lock().push(Arc::downgrade(socket));
    }

    /// Register this listening socket in the global TCP_LISTENERS table.
    /// Called from listen() after transitioning to Listening state.
    pub(crate) fn register_as_listener(&self) {
        let self_ptr = self as *const Self;
        let weak = {
            let sockets = crate::net::TCP_SOCKETS.lock();
            sockets.iter().find_map(|w| {
                w.upgrade().and_then(|s| {
                    if Arc::as_ptr(&s) == self_ptr {
                        Some(w.clone())
                    } else {
                        None
                    }
                })
            })
        };
        if let Some(weak) = weak {
            let mut listeners = crate::net::TCP_LISTENERS.lock();
            let already = listeners.iter().any(|w| {
                w.upgrade()
                    .map(|s| Arc::as_ptr(&s) == self_ptr)
                    .unwrap_or(false)
            });
            if !already {
                listeners.push(weak);
            }
        }
    }

    /// Check if this listening socket has a pending connection.
    /// Called unconditionally after every poll cycle.
    pub(crate) fn refresh_accept_ready_after_poll(&self) -> bool {
        let ready = {
            let inner = self.inner.lock();
            match &*inner {
                Inner::Listening(l) => l.has_pending_connection(),
                _ => false,
            }
        };

        if ready {
            self.accept_waiters
                .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
        }

        ready
    }

    /// 在 NET_INTERFACE.poll() 之后刷新各状态的事件
    pub fn update_io_events(&self) -> (usize, usize) {
        let previous = self.pollee.load(Ordering::Acquire);
        let inner = self.inner.lock();
        inner.update_io_events(&self.pollee);
        let current = self.pollee.load(Ordering::Acquire);
        (previous, current)
    }

    /// 唤醒所有等待队列（无差别，仅在 shutdown/close 时使用）
    pub fn wake_wait_queues(&self) {
        self.recv_waiters.notify_events_all(
            EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM | EPollEvent::EPOLLHUP,
        );
        self.send_waiters.notify_events_all(
            EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM | EPollEvent::EPOLLHUP,
        );
        self.connect_waiters
            .notify_events_all(EPollEvent::EPOLLOUT | EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP);
        self.accept_waiters.notify_events_all(
            EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM | EPollEvent::EPOLLHUP,
        );
    }

    /// 条件唤醒等待队列：仅当 smoltcp 状态表明对应的 I/O 操作可执行时才唤醒。
    /// 用于 poll 后的批量唤醒，避免无差别唤醒造成的活锁（connect 在 SynSent 被反复唤醒）。
    pub fn wake_if_ready(&self) {
        // EventWaitQueue callbacks are edge notifications.  Publish only bits
        // that became ready in this refresh; repeatedly notifying every socket
        // that remains writable would turn EPOLLET back into level-triggered
        // polling and can keep Tokio's reactor permanently runnable.
        let (previous, events) = self.update_io_events();
        let became_ready = events & !previous;

        // accept 等待者：Listening 收到了新连接
        let accept_events = EPollEvent::from_bits_truncate(
            became_ready & (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits(),
        );
        if !accept_events.is_empty() {
            self.accept_waiters.notify_events_all(accept_events);
        }

        // connect 等待者：连接已建立（EPOLLOUT）或被拒绝（EPOLLERR / EPOLLHUP）
        let connect_events = EPollEvent::from_bits_truncate(
            became_ready
                & (EPollEvent::EPOLLOUT | EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP).bits(),
        );
        if !connect_events.is_empty() {
            self.connect_waiters.notify_events_all(connect_events);
        }

        // recv 等待者：有数据可读、对端关闭或 socket 出错。通知载荷只能
        // 包含本次真实边沿；把整个候选掩码传下去会在普通 EPOLLIN 上伪造
        // EPOLLRDHUP，使 Tokio 将连接永久标记为 read-closed。
        let recv_events = EPollEvent::from_bits_truncate(
            became_ready
                & (EPollEvent::EPOLLIN
                    | EPollEvent::EPOLLRDNORM
                    | EPollEvent::EPOLLRDHUP
                    | EPollEvent::EPOLLHUP
                    | EPollEvent::EPOLLERR)
                    .bits(),
        );
        if !recv_events.is_empty() {
            self.recv_waiters.notify_events_at_most(recv_events, 1);
        }

        // send 等待者：发送缓冲从不可写转为可写，或 socket 关闭/出错。
        let send_events = EPollEvent::from_bits_truncate(
            became_ready
                & (EPollEvent::EPOLLOUT
                    | EPollEvent::EPOLLWRNORM
                    | EPollEvent::EPOLLHUP
                    | EPollEvent::EPOLLERR)
                    .bits(),
        );
        if !send_events.is_empty() {
            self.send_waiters.notify_events_at_most(send_events, 1);
        }
    }

    /// 从 Inner 中提取 Connecting 状态并最终化（通过 `Connecting::into_result`）。
    /// - 连接成功 → 转为 `Inner::Established`，返回 `Ok(0)`
    /// - 连接被拒 → 转为 `Inner::Init`（清理 smoltcp handle），返回 `Err(ECONNREFUSED)`
    /// - 仍在连接中 → 恢复原状态，返回 `Err(EAGAIN)`
    fn finish_connecting(&self) -> SyscallRet {
        let mut inner = self.inner.lock();
        let tmp = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        if let Inner::Connecting(connecting) = tmp {
            let (new_state, result) = connecting.into_result();
            *inner = new_state;
            result.map(|_| 0usize)
        } else {
            // 不是 Connecting 状态，恢复
            *inner = tmp;
            Err(SyscallErr::EAGAIN)
        }
    }

    pub(crate) fn publish_fast_established(&self, route: RouteSocketHandle, ifindex: u32) {
        self.fast_route_id.store(route.0, Ordering::Relaxed);
        self.fast_ifindex.store(ifindex, Ordering::Relaxed);
        self.fast_state.store(2, Ordering::Release);
    }

    fn invalidate_fast(&self) {
        self.fast_state.store(0, Ordering::Release);
    }

    fn fast_key_established(&self) -> Option<(RouteSocketHandle, u32)> {
        if self.fast_state.load(Ordering::Acquire) != 2 {
            return None;
        }
        let h = self.fast_route_id.load(Ordering::Relaxed);
        let ifidx = self.fast_ifindex.load(Ordering::Relaxed);
        if h == 0 || ifidx == 0 {
            return None;
        }
        Some((RouteSocketHandle(h), ifidx))
    }

    fn try_publish_fast_from_bound(&self) {
        let bound = self.bound.lock();
        let route = match bound.socket_handle {
            Some(h) => h,
            None => return,
        };
        let ifindex = bound.ifindex;
        drop(bound);
        self.publish_fast_established(route, ifindex);
    }
}

fn update_ready_bit(pollee: &AtomicUsize, bit: usize, ready: bool) {
    if ready {
        pollee.fetch_or(bit, Ordering::Relaxed);
    } else {
        pollee.fetch_and(!bit, Ordering::Relaxed);
    }
}

impl Socket for TcpSocket {
    /// 将 TCP socket 绑定到本地 IP 端点。
    ///
    /// # Semantics
    ///
    /// 规范化 IPv4-mapped IPv6 地址，构建 `IpListenEndpoint`，通过 `Inner::bind()`
    /// 在 smoltcp socket set 中注册绑定。记录绑定元数据到 `self.bound`（
    /// `bound_addr`/`bound_port`/`ifindex`）。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁并调用 `Inner::bind()`，释放锁后设置 `self.bound`。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：`endpoint` 不是 `Endpoint::Ip`
    /// - `EAFNOSUPPORT`：`ep.addr` 的版本与 socket 版本不匹配
    /// - 其他错误由 `Inner::bind()` 产生
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let ep = IpEndpoint::new(self.normalize_ipv4_mapped(ep.addr), ep.port);
        if !ep.addr.is_unspecified() && !self.addr_family_matches(ep.addr) {
            return Err(SyscallErr::EAFNOSUPPORT);
        }
        let listen_ep = if ep.addr.is_unspecified() {
            IpListenEndpoint {
                addr: None,
                port: ep.port,
            }
        } else {
            IpListenEndpoint {
                addr: Some(ep.addr),
                port: ep.port,
            }
        };
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        match new_inner.bind(listen_ep) {
            Ok(bound) => {
                if let Inner::Init(inner_init) = &bound {
                    if let Init::Bound { local, .. } = inner_init {
                        let ifindex =
                            crate::net::net_core::ifindex_for_local_addr(Some(local.addr));
                        // Lazy bind: socket not yet in SocketSet; record metadata only
                        self.bound.lock().ifindex = ifindex;
                        self.bound.lock().bound_addr = Some(local.addr);
                        self.bound.lock().bound_port = local.port;
                    }
                }
                *inner = bound;
                Ok(0)
            }
            Err((revert, err)) => {
                *inner = revert;
                Err(err)
            }
        }
    }

    /// 将 TCP socket 标记为监听状态。
    ///
    /// # Semantics
    ///
    /// 获取 `self.inner` 锁，调用 `Inner::listen(BACKLOG_SIZE)` 将 `Init` 转为
    /// `Listening` 状态。成功后注册到全局 `TCP_LISTENERS` 表，后续 poll 循环
    /// 会无条件扫描该表检查是否收到新连接（由 `wake_tcp_accept_waiters()` 驱动）。
    ///
    /// # Errors
    ///
    /// - 由 `Inner::listen()` 产生：若 socket 处于非 `Init` 状态则失败并恢复原状态
    fn listen(&self) -> SyscallRet {
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        match new_inner.listen(BACKLOG_SIZE as usize) {
            Ok(listening) => {
                let listen_addr = listening.listen_addr();
                let ifindex = crate::net::net_core::ifindex_for_local_addr(listen_addr.addr);
                if let Some(&handle) = listening.handles.first() {
                    self.bound
                        .lock()
                        .bind(handle, ifindex, listen_addr.addr, listen_addr.port);
                }
                *inner = Inner::Listening(listening);
                drop(inner);
                Self::register_as_listener(self);
                Ok(0)
            }
            Err((revert, err)) => {
                *inner = revert;
                Err(err)
            }
        }
    }

    /// 发起 TCP 连接到远程端点。
    ///
    /// # Semantics
    ///
    /// 通过 `Inner::connect()` 在 smoltcp 中创建新的 TCP 控制块并进入
    /// `Connecting` 状态。调用后立即做一次 `NET_INTERFACE.poll()` 并检查
    /// 握手是否已完成（`is_connected()` 或 `failure_reason()`）。
    /// 若已完成，内部 `finish_connecting()` 将状态转为 `Established` 并返回
    /// `Ok(0)`（同步连接成功）；否则返回 `Err(EAGAIN)`，上层 `sys_connect`
    /// 将根据阻塞/非阻塞模式分别进入 `WaitQueue` 或返回 `EINPROGRESS`。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁。连接失败（非 `EAGAIN` 错误）时直接设置 `pollee`
    /// 为 `EPOLLOUT|EPOLLERR` 位，让 poll 立即返回可写和错误事件。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：`endpoint` 不是 `Endpoint::Ip`
    /// - `EAFNOSUPPORT`：地址族不匹配
    /// - `EAGAIN`：握手未完成（正常流程，需要后续 `try_connect` 继续）
    /// - `ECONNREFUSED`/etc：由 `Inner::connect()` 产生
    ///
    /// # Linux Compatibility
    ///
    /// 与 Linux 6.6 一致：连接失败时 poll 返回 `EPOLLOUT|EPOLLERR`，
    /// 应用通过 `getsockopt(SO_ERROR)` 获取具体 errno。
    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        let Endpoint::Ip(ep) = endpoint else {
            return Err(SyscallErr::EINVAL);
        };
        let ep = IpEndpoint::new(self.normalize_ipv4_mapped(ep.addr), ep.port);
        if !self.addr_family_matches(ep.addr) {
            return Err(SyscallErr::EAFNOSUPPORT);
        }
        let remote_endpoint = if ep.addr.is_unspecified() {
            let loopback_addr = match ep.addr {
                IpAddress::Ipv4(_) => IpAddress::v4(127, 0, 0, 1),
                IpAddress::Ipv6(_) => IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1),
            };
            IpEndpoint::new(loopback_addr, ep.port)
        } else {
            ep
        };
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        let bound_ifindex_capture = *self.bound_ifindex.lock();
        match new_inner.connect(remote_endpoint, bound_ifindex_capture) {
            Ok(connecting) => {
                let ifindex = bound_ifindex_capture.unwrap_or_else(|| {
                    crate::net::net_core::ifindex_for_local_addr(Some(connecting.local.addr))
                });
                self.bound.lock().bind(
                    connecting.handle,
                    ifindex,
                    Some(connecting.local.addr),
                    connecting.local.port,
                );
                *inner = Inner::Connecting(connecting);
                drop(inner);
                // 做一次非阻塞状态检查
                NET_INTERFACE.poll();
                let inner = self.inner.lock();
                match &*inner {
                    Inner::Connecting(c) => {
                        c.update_io_events(&self.pollee);
                        // 连接已建立、连接被拒绝 → 都通过 finish_connecting 做状态转换
                        if c.is_connected() || c.failure_reason().is_some() {
                            drop(inner);
                            self.finish_connecting()
                        } else {
                            Err(SyscallErr::EAGAIN)
                        }
                    }
                    _ => Err(SyscallErr::EAGAIN),
                }
            }
            Err((revert, err)) => {
                *inner = revert;
                // 连接失败：设 pollee 让 poll 立即返回 EPOLLOUT|EPOLLERR
                if err != SyscallErr::EAGAIN {
                    self.pollee.store(
                        EPollEvent::EPOLLOUT.bits() | EPollEvent::EPOLLERR.bits(),
                        Ordering::Relaxed,
                    );
                }
                Err(err)
            }
        }
    }

    /// 非阻塞检查 TCP 握手进度——单次尝试，不睡眠。
    ///
    /// # Semantics
    ///
    /// `sys_connect` 的 `WaitQueue` 条件闭包和非阻塞路径都调用此方法。方法先
    /// 调用 `NET_INTERFACE.try_poll()` 推进 smoltcp 状态，然后查询底层 TCP
    /// state。若状态已是 `Established`/`CloseWait` 但 `Inner::Connecting` 的
    /// `result` 字段未更新，强制修正为 `ConnectResult::Connected`。
    ///
    /// 成功后调用 `finish_connecting()` 做状态转换并发布 fast path 键。
    /// `Closed` 状态（对端 RST）映射为 `ECONNREFUSED`。
    ///
    /// 普通 WaitQueue 在登记 entry 后会释放队列锁，再执行条件检查；因此
    /// `try_poll()` 触发本 socket 的可靠通知时不会重入同一队列锁。
    ///
    /// # Errors
    ///
    /// - `Ok(0)`：已连接
    /// - `ECONNREFUSED`：对端 RST
    /// - `EAGAIN`：仍在握手中
    fn try_connect(&self) -> Result<isize, SyscallErr> {
        NET_INTERFACE.try_poll();
        let inner = self.inner.lock();
        let ret = match &*inner {
            Inner::Connecting(c) => {
                let state = NET_INTERFACE
                    .tcp_routed_socket(c.handle, |s| s.state())
                    .unwrap_or(smoltcp::socket::tcp::State::Closed);
                let ready = c.update_io_events(&self.pollee);
                if c.is_connected()
                    || (ready && c.failure_reason().is_some())
                    || matches!(
                        state,
                        smoltcp::socket::tcp::State::Established
                            | smoltcp::socket::tcp::State::CloseWait
                    )
                {
                    // 如果 state 已经是已连接状态，但 result 还是 Connecting，强制修正
                    if matches!(
                        state,
                        smoltcp::socket::tcp::State::Established
                            | smoltcp::socket::tcp::State::CloseWait
                    ) && !c.is_connected()
                    {
                        *c.result.lock() = ConnectResult::Connected;
                    }
                    drop(inner);
                    let ret = self.finish_connecting().map(|v| v as isize);
                    if ret.is_ok() {
                        self.try_publish_fast_from_bound();
                    }
                    ret
                } else if state == smoltcp::socket::tcp::State::Closed {
                    drop(inner);
                    let _ = self.finish_connecting(); // 转换状态以触发正确的事件
                    Err(SyscallErr::ECONNREFUSED)
                } else {
                    Err(SyscallErr::EAGAIN)
                }
            }
            Inner::Established(_) => Ok(0),
            _ => Err(SyscallErr::EAGAIN),
        };
        ret
    }

    fn take_error(&self) -> Option<SyscallErr> {
        NET_INTERFACE.try_poll();
        let mut inner = self.inner.lock();
        match &mut *inner {
            Inner::Init(Init::Bound { pending_error, .. }) => pending_error.take(),
            _ => None,
        }
    }

    /// 从监听 socket 获取下一个已完成的连接。
    ///
    /// # Semantics
    ///
    /// 仅在 `Listening` 状态下调用 `Inner::accept()`，获取 `Established` 的
    /// `Inner` 和 peer `IpEndpoint`。基于此创建新的 `TcpSocket`（设置 `BoundInner`
    /// 元数据）并注册到全局 `TCP_SOCKETS` 表（否则 `pselect`/`epoll` 永远不会
    /// 触发该新 socket 的事件）。然后分配新 fd，若 `addr != 0` 则写回 peer 地址。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁。新 socket 由 `Self::register_tcp_socket()` 加入
    /// 全局 `TCP_SOCKETS` 表（持有对应的全局互斥锁）。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：非监听 socket
    /// - `EAGAIN`：无可用连接
    /// - `EMFILE`：fd 表已满
    ///
    /// # Linux Compatibility
    ///
    /// 简化实现：无独立的 SYN backlog 队列。`Listening` 状态下的 accept 无条件
    /// 尝试从 smoltcp 的 `tcp_accept` 获取下一个连接，而非维护 backlog 计数。
    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        if !matches!(&*inner, Inner::Listening(_)) {
            return Err(SyscallErr::EINVAL);
        }
        let (connected_inner, peer_endpoint) = match inner.accept() {
            Ok(result) => result,
            Err(e) => return Err(e),
        };

        let mut fast_route: Option<RouteSocketHandle> = None;
        let mut fast_ifindex: u32 = 0;

        let accepted_bound = if let Inner::Established(ref est) = connected_inner {
            if let Some(binding) = NET_INTERFACE
                .inner_handler(|inner_ref| inner_ref.bindings.get(&est.handle).copied())
                .flatten()
            {
                fast_route = Some(est.handle);
                fast_ifindex = binding.ifindex;
            }
            let ifindex = fast_ifindex;
            let mut b = BoundInner::new();
            b.bind(est.handle, ifindex, Some(est.local.addr), est.local.port);
            b
        } else {
            BoundInner::new()
        };

        let connected_socket = Arc::new(TcpSocket {
            inner: Mutex::new(connected_inner),
            pollee: AtomicUsize::new(0),
            read_shutdown: AtomicBool::new(false),
            write_shutdown: AtomicBool::new(false),
            reuse_addr: AtomicBool::new(false),
            multicast_group_joined: AtomicBool::new(false),
            bound: Mutex::new(accepted_bound),
            bound_ifindex: Mutex::new(None),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            connect_waiters: EventWaitQueue::new(),
            accept_waiters: EventWaitQueue::new(),
            ip_version: self.ip_version,
            ipv6_v6only: AtomicBool::new(self.ipv6_v6only.load(Ordering::Acquire)),
            fast_route_id: AtomicUsize::new(0),
            fast_ifindex: AtomicU32::new(0),
            fast_state: AtomicU8::new(0),
        });

        if let Some(handle) = fast_route {
            connected_socket.publish_fast_established(handle, fast_ifindex);
        }

        // 新 accept 的连接也必须注册到全局 TCP_SOCKETS，否则 pselect/epoll 永远等不到事件
        Self::register_tcp_socket(&connected_socket);

        let socket_file: Arc<dyn crate::fs::vfs::IndexNode> =
            Arc::new(SocketFile::new(connected_socket));

        let task = current_task().unwrap();
        let files_ref = task.process.files();
        let mut fd_table = files_ref.lock();
        let old_cloexec = fd_table.get_cloexec(sockfd as usize);
        let vf = vfs::File::new_without_open(socket_file, FileFlags::O_RDWR, vfs::FileType::Socket);
        let new_fd = fd_table
            .alloc_fd(vf, old_cloexec)
            .map_err(|_| SyscallErr::EMFILE)?;

        // addr == 0 means user doesn't care about peer address (POSIX allows this)
        if addr != 0 {
            Endpoint::Ip(peer_endpoint).fill_sockaddr(addr, addrlen)?;
        }

        Ok(new_fd)
    }

    fn socket_type(&self) -> PSOCK {
        PSOCK::Stream
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recv_buffer_size()
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().send_buffer_size()
    }

    fn set_recv_buf_size(&self, size: usize) {
        let mut inner = self.inner.lock();
        match &mut *inner {
            Inner::Init(init) => {
                // Only resize RX; keep TX at default (64KB)
                let _ = init.resize_buffers(size, 64 * 1024);
            }
            Inner::Established(e) => {
                // 此 smoltcp 版本不支持动态 recv buffer 调整
                let _ = size;
            }
            Inner::Connecting(c) => {
                let _ = c;
            }
            Inner::SelfConnected(sc) => {
                sc.rx_cap.store(size, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn set_send_buf_size(&self, size: usize) {
        let mut inner = self.inner.lock();
        match &mut *inner {
            Inner::Init(init) => {
                // Only resize TX; keep RX at default (64KB)
                let _ = init.resize_buffers(64 * 1024, size);
            }
            Inner::Established(e) => {
                // 此 smoltcp 版本不支持动态 send buffer 调整
                let _ = size;
            }
            Inner::Connecting(c) => {
                let _ = c;
            }
            _ => {}
        }
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        Some(Endpoint::Ip(self.inner.lock().local_endpoint()))
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        self.inner.lock().remote_endpoint().map(Endpoint::Ip)
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        let inner = self.inner.lock();
        let result = inner.shutdown(how);
        drop(inner);
        if result.is_ok() {
            match how {
                0 => self.read_shutdown.store(true, Ordering::Release), // SHUT_RD
                1 => self.write_shutdown.store(true, Ordering::Release), // SHUT_WR
                _ => {
                    self.read_shutdown.store(true, Ordering::Release);
                    self.write_shutdown.store(true, Ordering::Release); // SHUT_RDWR
                }
            }
            // shutdown is itself the state transition.  Do not rely on a later
            // network poll to repeat a level-like notification: EPOLLET users
            // and blocked send/recv calls must be woken immediately.
            self.wake_wait_queues();
        }
        result
    }

    fn set_nagle_enabled(&self, enabled: bool) -> SyscallRet {
        self.inner.lock().set_nagle_enabled(enabled);
        Ok(0)
    }

    fn set_keep_alive(&self, enabled: bool) -> SyscallRet {
        self.inner.lock().set_keep_alive(enabled);
        Ok(0)
    }

    fn reuse_addr(&self) -> SyscallRet {
        Ok(self.reuse_addr.load(Ordering::Acquire) as usize)
    }

    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet {
        self.reuse_addr.store(enabled, Ordering::Release);
        Ok(0)
    }

    fn set_bind_to_device(&self, ifname: &str) -> SyscallRet {
        if ifname.is_empty() {
            *self.bound_ifindex.lock() = None;
            log::info!("[TcpSocket] unbound from device");
            return Ok(0);
        }
        let ns = crate::net::net_core::current_netns();
        let list = ns.device_list.lock();
        let iface = list.values().find(|d| d.iface_name() == ifname);
        match iface {
            Some(iface) => {
                *self.bound_ifindex.lock() = Some(iface.nic_id() as u32);
                log::info!(
                    "[TcpSocket] bound to device {} (ifindex={})",
                    ifname,
                    iface.nic_id()
                );
                Ok(0)
            }
            None => Err(SyscallErr::ENODEV),
        }
    }

    fn join_multicast_group(&self) -> SyscallRet {
        self.multicast_group_joined.store(true, Ordering::Release);
        Ok(0)
    }

    fn leave_multicast_group(&self) -> SyscallRet {
        if self.multicast_group_joined.swap(false, Ordering::AcqRel) {
            Ok(0)
        } else {
            Err(SyscallErr::EADDRNOTAVAIL)
        }
    }

    fn send_to(&self, _buf: &[u8], _dest: Endpoint) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 非阻塞尝试从 TCP socket 接收数据（单次，不 poll）。
    ///
    /// # Semantics
    ///
    /// 两条路径：
    /// 1. **Fast path**（`fast_key_established()` 返回有效键）：直接通过
    ///    `NET_INTERFACE.tcp_routed_socket()` 调用 smoltcp `recv_slice`，
    ///    无需获取 `self.inner` 锁。若 `pollee` 缓存未设置 `EPOLLIN` 位，
    ///    先做 `try_poll_stack(ifindex)` 确保 smoltcp 数据到达。
    /// 2. **Slow path**：获取 `self.inner` 锁，调用 `Inner::try_recv()`。
    ///
    /// **阻塞模型**：此方法是 `try_xxx` 模式——不做 poll、不睡眠、不调度。
    /// 调用者（`sys_recvfrom`）负责在调用前后轮询并管理 `WaitQueue` 交互。
    ///
    /// **边界情况**：`read_shutdown` 标志设置时返回 `Ok(0)`（EOF/Linux 语义）。
    ///
    /// # Locking
    ///
    /// Slow path 获取 `self.inner` 锁（spin Mutex）。Fast path 无锁，仅读取
    /// `Atomic` 变量并做安全检查。
    ///
    /// # Errors
    ///
    /// - `Ok(0)`：FIN 已接收或 `read_shutdown`
    /// - `ECONNRESET`：连接被对端 Reset
    /// - `EAGAIN`：无数据可用
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let fast = self.fast_key_established();
        if self.pollee.load(Ordering::Relaxed) & EPollEvent::EPOLLIN.bits() == 0 {
            if let Some((_route, ifindex)) = fast {
                NET_INTERFACE.try_poll_stack(ifindex);
            } else {
                NET_INTERFACE.try_poll();
            }
        }
        if self.read_shutdown.load(Ordering::Acquire) {
            let ret = Ok(0);
            #[cfg(feature = "net_perf_diag")]
            record_tcp_recv_perf(buf.len(), &ret);
            return ret;
        }

        if let Some((route, _ifindex)) = fast {
            if let Some((ret, ready_after)) = NET_INTERFACE.tcp_routed_socket(route, |tcp_sock| {
                let result = if tcp_sock.can_recv() {
                    tcp_sock
                        .recv_slice(buf)
                        .map(|n| n as isize)
                        .map_err(|_| SyscallErr::ENOTCONN)
                } else {
                    match tcp_sock.state() {
                        tcp::State::CloseWait
                        | tcp::State::Closing
                        | tcp::State::LastAck
                        | tcp::State::TimeWait => Ok(0),
                        tcp::State::Closed => Err(SyscallErr::ECONNRESET),
                        _ if !tcp_sock.may_recv() => Ok(0),
                        _ => Err(SyscallErr::EAGAIN),
                    }
                };
                let readable = tcp_sock.can_recv()
                    || !tcp_sock.may_recv()
                    || matches!(
                        tcp_sock.state(),
                        tcp::State::CloseWait
                            | tcp::State::Closing
                            | tcp::State::LastAck
                            | tcp::State::TimeWait
                            | tcp::State::Closed
                    );
                (result, readable)
            }) {
                update_ready_bit(&self.pollee, EPollEvent::EPOLLIN.bits(), ready_after);
                #[cfg(feature = "net_perf_diag")]
                record_tcp_recv_perf(buf.len(), &ret);
                return ret;
            }
            self.invalidate_fast();
        }

        let inner = self.inner.lock();
        let ret = inner.try_recv(buf);
        drop(inner);
        // A successful short read may have drained the receive queue.  Derive
        // readiness from the socket after I/O instead of treating every
        // successful return as still readable; otherwise EPOLLET users never
        // observe the next empty -> readable transition.
        self.update_io_events();
        #[cfg(feature = "net_perf_diag")]
        record_tcp_recv_perf(buf.len(), &ret);
        ret
    }

    /// 非阻塞尝试向 TCP socket 发送数据（单次，不 poll）。
    ///
    /// # Semantics
    ///
    /// 两条路径：
    /// 1. **Fast path**：通过 `NET_INTERFACE.tcp_routed_socket()` 直接写入
    ///    smoltcp，无需 `self.inner` 锁。发送前若 `pollee` 缓存未设 `EPOLLOUT`
    ///    位，先 `try_poll_stack(ifindex)` 推进 TCP TX 窗口。
    /// 2. **Slow path**：获取 `self.inner` 锁，调用 `Inner::try_send()`。
    ///    如果 socket 仍在 `Connecting` 状态，在发送前调用 `try_connect()`
    ///    完成握手（Linux TCP 语义允许在 `connect` 完全建立前发送数据）。
    ///
    /// **阻塞模型**：`try_xxx` 模式——不 poll、不睡眠、不调度。
    ///
    /// **边界情况**：`write_shutdown` → `EPIPE`（`SIGPIPE` 由 `sys_sendto`/`sys_write` 处理）。
    ///
    /// # Locking
    ///
    /// Fast path 无锁。Slow path 获取 `self.inner` 锁，且可能递归调用
    /// `try_connect()`（后者持有自己的 `inner` 锁——`TicketMutex` 非重入，
    /// 因此确保两次锁之间的中间状态不会冲突）。
    ///
    /// # Errors
    ///
    /// - `EPIPE`：`write_shutdown` 或对端 RESET
    /// - `ECONNRESET`：连接被对端 Reset
    /// - `EAGAIN`：发送缓冲满
    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let fast = self.fast_key_established();
        if self.pollee.load(Ordering::Relaxed) & EPollEvent::EPOLLOUT.bits() == 0 {
            if let Some((_route, ifindex)) = fast {
                NET_INTERFACE.try_poll_stack(ifindex);
            } else {
                NET_INTERFACE.try_poll();
            }
        }
        if self.write_shutdown.load(Ordering::Acquire) {
            return Err(SyscallErr::EPIPE);
        }

        let is_connecting = {
            let inner = self.inner.lock();
            matches!(&*inner, Inner::Connecting(_))
        };
        if is_connecting {
            let _ = self.try_connect();
        }

        if let Some((route, _ifindex)) = fast {
            if let Some((ret, ready_after)) = NET_INTERFACE.tcp_routed_socket(route, |tcp_sock| {
                let result = if tcp_sock.can_send() {
                    tcp_sock
                        .send_slice(buf)
                        .map(|n| n as isize)
                        .map_err(|_| SyscallErr::ECONNABORTED)
                } else {
                    match tcp_sock.state() {
                        tcp::State::Closed => Err(SyscallErr::ECONNRESET),
                        tcp::State::TimeWait | tcp::State::Closing | tcp::State::LastAck => {
                            Err(SyscallErr::EPIPE)
                        }
                        _ => Err(SyscallErr::EAGAIN),
                    }
                };
                let writable = tcp_sock.can_send()
                    && !matches!(
                        tcp_sock.state(),
                        tcp::State::Closed
                            | tcp::State::TimeWait
                            | tcp::State::Closing
                            | tcp::State::LastAck
                    );
                (result, writable)
            }) {
                update_ready_bit(&self.pollee, EPollEvent::EPOLLOUT.bits(), ready_after);
                return ret;
            }
            self.invalidate_fast();
        }

        let inner = self.inner.lock();
        let ret = inner.try_send(buf);
        drop(inner);
        // A partial write can fill the send queue even though it succeeded.
        // Refresh from smoltcp so a later writable transition produces an edge.
        self.update_io_events();
        ret
    }

    fn try_recv_user(&self, buf: &mut UserBuffer, flags: MsgFlags) -> Result<isize, SyscallErr> {
        if self.pollee.load(Ordering::Relaxed) & EPollEvent::EPOLLIN.bits() == 0 {
            NET_INTERFACE.try_poll();
        }
        if self.read_shutdown.load(Ordering::Acquire) {
            let ret = Ok(0);
            #[cfg(feature = "net_perf_diag")]
            record_tcp_recv_perf(buf.len(), &ret);
            return ret;
        }
        let _ = flags;
        // 先在 socket 锁域内接收到 kernel buffer，释放锁后再进入 faultable uaccess。
        // 这避免形成 socket -> VM 的嵌套锁序。
        let total = buf.len().min(crate::hal::IO_CHUNK_SIZE);
        if total == 0 {
            return Ok(0);
        }
        let mut tmp = alloc::vec![0u8; total];
        let ret = self.try_recv(&mut tmp).and_then(|n| {
            if n <= 0 {
                return Ok(n);
            }
            buf.write_from(&tmp[..n as usize])
                .map(|copied| copied as isize)
                .map_err(|_| SyscallErr::EFAULT)
        });
        // sys_read/readv use this direct UserBuffer path.  In particular,
        // Tokio's TcpStream reads land here, and Tokio clears its userspace
        // readiness after a short read.  Mirror the actual post-read socket
        // state so the next packet can generate a fresh EPOLLET notification.
        self.update_io_events();
        #[cfg(feature = "net_perf_diag")]
        record_tcp_recv_perf(buf.len(), &ret);
        ret
    }

    fn try_send_user(&self, buf: &UserBuffer, flags: MsgFlags) -> Result<isize, SyscallErr> {
        if self.pollee.load(Ordering::Relaxed) & EPollEvent::EPOLLOUT.bits() == 0 {
            NET_INTERFACE.try_poll();
        }
        if self.write_shutdown.load(Ordering::Acquire) {
            return Err(SyscallErr::EPIPE);
        }
        let is_connecting = {
            let inner = self.inner.lock();
            matches!(&*inner, Inner::Connecting(_))
        };
        if is_connecting {
            let _ = self.try_connect();
        }
        let total = buf.len().min(crate::hal::IO_CHUNK_SIZE);
        if total == 0 {
            let inner = self.inner.lock();
            let ret = inner.try_send(&[]);
            drop(inner);
            self.update_io_events();
            return ret;
        }
        let mut tmp = alloc::vec![0u8; total];
        let n = buf
            .read_into_at(0, &mut tmp)
            .map_err(|_| SyscallErr::EFAULT)?;
        let inner = self.inner.lock();
        let ret = inner.try_send(&tmp[..n]);
        drop(inner);
        // Keep the producer-side readiness cache aligned with the real send
        // queue; syscall success alone does not imply that it remains writable.
        self.update_io_events();
        ret
    }

    fn socket_r_ready(&self) -> bool {
        self.update_io_events();
        log::debug!(
            "[TcpSocket]Checking if socket is ready for reading, pollee: {}",
            self.pollee.load(Ordering::Acquire)
        );
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLIN.bits() != 0
    }

    fn socket_w_ready(&self) -> bool {
        self.update_io_events();
        log::debug!(
            "[TcpSocket]Checking if socket is ready for writing, pollee: {}",
            self.pollee.load(Ordering::Acquire)
        );
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLOUT.bits() != 0
    }

    fn socket_hang_up(&self) -> bool {
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLHUP.bits() != 0
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.recv_waiters.wait_queue())
    }

    fn recv_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.send_waiters.wait_queue())
    }

    fn send_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.send_waiters)
    }

    fn connect_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.connect_waiters.wait_queue())
    }

    fn connect_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.connect_waiters)
    }

    fn accept_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.accept_waiters.wait_queue())
    }

    fn accept_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.accept_waiters)
    }

    fn tcp_state(&self) -> Option<u8> {
        Some(self.inner.lock().tcp_state_code())
    }

    fn set_ipv6_v6only(&self, enabled: bool) -> SyscallRet {
        if self.ip_version != IpVersion::Ipv6 {
            return Err(SyscallErr::ENOPROTOOPT);
        }
        self.ipv6_v6only.store(enabled, Ordering::Release);
        Ok(0)
    }
}

// Safety: `TcpSocket` 所有字段均为线程安全类型：
//   - `Mutex<Inner>` 保护内部 TCP 状态机（smoltcp handles），所有访问经过 locking
//   - `AtomicBool` / `AtomicUsize` / `AtomicU32` / `AtomicU8` 提供无锁同步
//   - `EventWaitQueue` 内部使用 `Mutex` 保护等待队列
// 由于单核 `Arc<dyn Socket>` 共享，`Send` + `Sync` 允许在任务间传递 Arc 引用，
// 不会导致数据竞争。
unsafe impl Send for TcpSocket {}
// Safety: 同上，`TcpSocket` 的所有可变状态通过 `Mutex` 和 `Atomic*` 安全共享。
unsafe impl Sync for TcpSocket {}

impl Drop for TcpSocket {
    fn drop(&mut self) {
        self.invalidate_fast();
        {
            let inner = self.inner.lock();
            let state_name = match &*inner {
                Inner::Init(_) => "Init",
                Inner::Connecting(_) => "Connecting",
                Inner::Listening(_) => "Listening",
                Inner::Established(_) => "Established",
                Inner::SelfConnected(_) => "SelfConnected",
                Inner::Closed(_) => "Closed",
            };
            log::info!("[TcpSocket::drop] state={}", state_name);
            inner.close();
        }
        // 设置 pollee 为对端关闭/错误事件，让 epoll/select 立即可读并报 HUP
        self.pollee.store(
            (EPollEvent::EPOLLIN
                | EPollEvent::EPOLLRDNORM
                | EPollEvent::EPOLLHUP
                | EPollEvent::EPOLLRDHUP)
                .bits(),
            Ordering::Release,
        );

        // 唤醒所有阻塞在该 socket 上的系统调用（recvfrom/accept/connect/pselect）
        self.wake_wait_queues();
    }
}
