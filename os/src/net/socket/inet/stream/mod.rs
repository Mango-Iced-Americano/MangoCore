//! TCP Stream Socket —— TcpStreamSocket
//!
//! 架构对标 DragonOS `net/socket/inet/stream/mod.rs`。
//! 使用 6 状态 Inner 枚举管理 TCP 状态机：
//!   Init / Connecting / Listening / Established / SelfConnected / Closed
//!
//! TcpStreamSocket 包装：
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
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
use spin::Mutex;

use crate::net::{address, config::NET_INTERFACE, Socket, SocketFile, SocketType};
use crate::{
    fs::FileDescriptor,
    task::{current_task, WaitQueue},
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};

use self::inner::{
    with_tcp_mut, Connecting, EPollEvent, Established, Init, Listening, SelfConnected, BACKLOG_SIZE,
};
use crate::net::socket::inet::common::PortManager;
use crate::net::socket::inet::stream::inner::ConnectResult;
use crate::trace_event;
/// TCP Stream Socket —— 对外表现为 Socket trait
pub struct TcpStreamSocket {
    pub inner: Mutex<Inner>,
    pub pollee: AtomicUsize,
    /// 读端已关闭（SHUT_RD）
    pub read_shutdown: AtomicBool,
    /// 写端已关闭（SHUT_WR）
    pub write_shutdown: AtomicBool,
    pub reuse_addr: AtomicBool,
    pub recv_waiters: Mutex<WaitQueue>,
    pub send_waiters: Mutex<WaitQueue>,
    pub connect_waiters: Mutex<WaitQueue>,
    pub accept_waiters: Mutex<WaitQueue>,
}

impl TcpStreamSocket {
    /// 创建一个新的 TCP socket（默认 IPv4）
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::Init(Init::new(smoltcp::wire::IpVersion::Ipv4))),
            pollee: AtomicUsize::new(0),
            read_shutdown: AtomicBool::new(false),
            write_shutdown: AtomicBool::new(false),
            reuse_addr: AtomicBool::new(false),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        }
    }

    /// 注册到全局 TCP_SOCKETS 表
    pub fn register_tcp_socket(socket: &Arc<Self>) {
        crate::net::TCP_SOCKETS.lock().push(Arc::downgrade(socket));
    }

    /// 在 NET_INTERFACE.poll() 之后刷新各状态的事件
    pub fn update_io_events(&self) {
        // NET_INTERFACE.try_poll();
        let inner = self.inner.lock();
        inner.update_io_events(&self.pollee);
    }

    /// 唤醒所有等待队列（无差别，仅在 shutdown/close 时使用）
    pub fn wake_wait_queues(&self) {
        self.recv_waiters.lock().wake_all();
        self.send_waiters.lock().wake_all();
        self.connect_waiters.lock().wake_all();
        self.accept_waiters.lock().wake_all();
    }

    /// 条件唤醒等待队列：仅当 smoltcp 状态表明对应的 I/O 操作可执行时才唤醒。
    /// 用于 poll 后的批量唤醒，避免无差别唤醒造成的活锁（connect 在 SynSent 被反复唤醒）。
    pub fn wake_if_ready(&self) {
        // 先同步 pollee 缓存的 IO 事件
        self.update_io_events();
        let events = self.pollee.load(Ordering::Acquire);

        // accept 等待者：Listening 收到了新连接
        if !self.accept_waiters.lock().is_empty() {
            if events & (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM).bits() != 0 {
                self.accept_waiters.lock().wake_all();
            }
        }

        // connect 等待者：连接已建立（EPOLLOUT）或被拒绝（EPOLLERR / EPOLLHUP）
        if !self.connect_waiters.lock().is_empty() {
            if events & (EPollEvent::EPOLLOUT | EPollEvent::EPOLLERR | EPollEvent::EPOLLHUP).bits()
                != 0
            {
                self.connect_waiters.lock().wake_all();
            }
        }

        // recv 等待者：有数据可读或对端关闭
        if !self.recv_waiters.lock().is_empty() {
            if events
                & (EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM | EPollEvent::EPOLLRDHUP).bits()
                != 0
            {
                self.recv_waiters.lock().wake_at_most(1);
            }
        }

        // send 等待者：可发送或写端已 shutdown（shutdown 时 send 会返回 EPIPE）
        if !self.send_waiters.lock().is_empty() {
            if events & (EPollEvent::EPOLLOUT | EPollEvent::EPOLLWRNORM).bits() != 0
                || self.write_shutdown.load(Ordering::Acquire)
            {
                self.send_waiters.lock().wake_at_most(1);
            }
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
}

impl Socket for TcpStreamSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        match new_inner.bind(addr) {
            Ok(bound) => {
                *inner = bound;
                Ok(0)
            }
            Err((revert, err)) => {
                *inner = revert;
                Err(err)
            }
        }
    }

    fn listen(&self) -> SyscallRet {
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        match new_inner.listen(BACKLOG_SIZE as usize) {
            Ok(listening) => {
                *inner = Inner::Listening(listening);
                Ok(0)
            }
            Err((revert, err)) => {
                *inner = revert;
                Err(err)
            }
        }
    }

    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> SyscallRet {
        let remote_endpoint = address::endpoint(addr_buf)?;
        let mut inner = self.inner.lock();
        let new_inner = core::mem::replace(
            &mut *inner,
            Inner::Closed(Closed::new(smoltcp::wire::IpVersion::Ipv4)),
        );
        match new_inner.connect(remote_endpoint) {
            Ok(connecting) => {
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
                Err(err)
            }
        }
    }

    fn try_connect(&self) -> Result<isize, SyscallErr> {
        // NET_INTERFACE.poll();
        let inner = self.inner.lock();
        let ret = match &*inner {
            Inner::Connecting(c) => {
                let state = NET_INTERFACE
                    .tcp_socket(c.handle, |s| s.state())
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
                    self.finish_connecting().map(|v| v as isize)
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

    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet {
        // NET_INTERFACE.poll();
        let mut inner = self.inner.lock();
        if !matches!(&*inner, Inner::Listening(_)) {
            return Err(SyscallErr::EINVAL);
        }
        let (connected_inner, peer_endpoint) = match inner.accept() {
            Ok(result) => result,
            Err(e) => return Err(e),
        };

        let connected_socket = Arc::new(TcpStreamSocket {
            inner: Mutex::new(connected_inner),
            pollee: AtomicUsize::new(0),
            read_shutdown: AtomicBool::new(false),
            write_shutdown: AtomicBool::new(false),
            reuse_addr: AtomicBool::new(false),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        });

        // 新 accept 的连接也必须注册到全局 TCP_SOCKETS，否则 pselect/epoll 永远等不到事件
        Self::register_tcp_socket(&connected_socket);

        let socket_file = Arc::new(SocketFile::new(connected_socket));

        let task = current_task().unwrap();
        let mut fd_table = task.files.lock();
        let old_cloexec = fd_table
            .get_ref(sockfd as usize)
            .map(|fd| fd.get_cloexec())
            .unwrap_or(false);
        let new_fd = fd_table
            .insert(FileDescriptor::new(old_cloexec, false, socket_file))
            .map_err(|_| SyscallErr::EMFILE)?;

        address::fill_with_endpoint(peer_endpoint, addr, addrlen)?;

        Ok(new_fd)
    }

    fn socket_type(&self) -> SocketType {
        SocketType::SOCK_STREAM
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
                let _ = init.resize_buffers(size, size);
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
                let _ = init.resize_buffers(size, size);
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

    fn local_endpoint(&self) -> IpListenEndpoint {
        let ep = self.inner.lock().local_endpoint();
        IpListenEndpoint::from(ep)
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        self.inner.lock().remote_endpoint()
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        let inner = self.inner.lock();
        let result = inner.shutdown(how);
        if result.is_ok() {
            match how {
                0 => self.read_shutdown.store(true, Ordering::Release), // SHUT_RD
                1 => self.write_shutdown.store(true, Ordering::Release), // SHUT_WR
                _ => {
                    self.read_shutdown.store(true, Ordering::Release);
                    self.write_shutdown.store(true, Ordering::Release); // SHUT_RDWR
                }
            }
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

    fn send_to(&self, _buf: &[u8], _dest_addr: IpEndpoint) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        if self.read_shutdown.load(Ordering::Acquire) {
            return Ok(0); // EOF after read shutdown
        }
        let inner = self.inner.lock();
        inner.try_recv(buf)
    }

    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        if self.write_shutdown.load(Ordering::Acquire) {
            return Err(SyscallErr::EPIPE);
        }
        let inner = self.inner.lock();
        inner.try_send(buf)
    }

    fn socket_r_ready(&self) -> bool {
        self.update_io_events();
        log::debug!(
            "[TcpStreamSocket]Checking if socket is ready for reading, pollee: {}",
            self.pollee.load(Ordering::Acquire)
        );
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLIN.bits() != 0
    }

    fn socket_w_ready(&self) -> bool {
        self.update_io_events();
        log::debug!(
            "[TcpStreamSocket]Checking if socket is ready for writing, pollee: {}",
            self.pollee.load(Ordering::Acquire)
        );
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLOUT.bits() != 0
    }

    fn socket_hang_up(&self) -> bool {
        self.pollee.load(Ordering::Acquire) & EPollEvent::EPOLLHUP.bits() != 0
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.send_waiters)
    }

    fn connect_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.connect_waiters)
    }

    fn accept_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.accept_waiters)
    }

    fn tcp_state(&self) -> Option<u8> {
        Some(self.inner.lock().tcp_state_code())
    }
}

unsafe impl Send for TcpStreamSocket {}
unsafe impl Sync for TcpStreamSocket {}

impl Drop for TcpStreamSocket {
    fn drop(&mut self) {
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
            log::info!("[TcpStreamSocket::drop] state={}", state_name);
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
