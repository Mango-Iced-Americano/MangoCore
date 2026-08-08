//! Unix Stream Socket 实现
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/stream/mod.rs` 设计。
//!
//! # Implementation Status
//!
//! `bind`（Path/Abstract/Unnamed）、`listen`（backlog=16）、`connect`（创建 `Connected` 对
//! 并推入 listener 的 `incoming` 队列）、`accept`（pop `incoming` 并包装为新 fd）、
//! `try_recv`/`try_send`（通过 `Connected` 的 `RingBuffer` pair）、`shutdown`、
//! epoll 事件和 `wake_wait_queues` 已实现。
//!
//! # Limitations
//!
//! - `shutdown` 仅对 `Connected` 态生效，`Init`/`Listener` 返回 `ENOTCONN`
//! - 不支持 `SCM_RIGHTS` 等 ancillary data
//! - `backlog` 固定为 16

pub mod inner;
use self::inner::Connected;
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::fs::vfs::{self, FileFlags};
use crate::net::socket::unix::ns::{ABSTRACT_TABLE, UNIX_PATH_MAX};
use crate::net::socket::unix::PATH_TABLE;
use crate::net::socket::unix::{UnixEndpoint, UnixEndpointBound};
use crate::net::syscall::common::MsgFlags;
use crate::net::{Endpoint, Socket, PSOCK, SHUT_RD};
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use self::inner::{Init, Inner, Listener, UNIX_STREAM_DEFAULT_BUF_SIZE};

/// Unix Stream Socket
pub struct UnixStreamSocket {
    /// 内部状态机
    pub inner: Mutex<Inner>,
    /// 是否非阻塞
    is_nonblock: AtomicBool,
    /// 接收缓冲区大小
    recv_buf_size: AtomicUsize,
    /// 发送缓冲区大小
    send_buf_size: AtomicUsize,
    /// 等待队列
    pub recv_waiters: Arc<EventWaitQueue>,
    pub send_waiters: Arc<EventWaitQueue>,
    pub connect_waiters: Arc<EventWaitQueue>,
    pub accept_waiters: Arc<EventWaitQueue>,
}

impl core::fmt::Debug for UnixStreamSocket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnixStreamSocket")
            .field("inner", &self.inner)
            .field("is_nonblock", &self.is_nonblock)
            .finish()
    }
}

impl UnixStreamSocket {
    pub fn new(is_nonblock: bool) -> Self {
        Self {
            inner: Mutex::new(Inner::Init(Init::new())),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            send_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            recv_waiters: Arc::new(EventWaitQueue::new()),
            send_waiters: Arc::new(EventWaitQueue::new()),
            connect_waiters: Arc::new(EventWaitQueue::new()),
            accept_waiters: Arc::new(EventWaitQueue::new()),
        }
    }

    pub fn new_connected(connected: Connected, is_nonblock: bool) -> Self {
        let recv_waiters = connected.recv_waiters.clone();
        let send_waiters = connected.send_waiters.clone();
        Self {
            inner: Mutex::new(Inner::Connected(connected)),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            send_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            recv_waiters,
            send_waiters,
            connect_waiters: Arc::new(EventWaitQueue::new()),
            accept_waiters: Arc::new(EventWaitQueue::new()),
        }
    }

    fn is_nonblocking(&self) -> bool {
        self.is_nonblock.load(Ordering::Relaxed)
    }

    /// 唤醒全部四个等待队列，通知所有阻塞者重新检查 readiness。
    ///
    /// # 触发场景
    ///
    /// | 队列 | 唤醒场景 |
    /// |------|---------|
    /// | `recv_waiters` | 对端 `try_send` 成功将数据推入本端 `rx` → 数据可读 |
    /// | `send_waiters` | 本端 `try_recv` 消费数据 → 释放 `tx` buffer 空间 → 对端可继续写入 |
    /// | `connect_waiters` | 连接建立完成（`Inner` 进入 `Connected` 态） |
    /// | `accept_waiters` | 新 `Connected` 被推入 listener 的 `incoming` 队列 |
    ///
    /// `shutdown` 路径也在设置标志后调用本方法（通过 `EPOLLHUP` 通知对端挂断）。
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
}

impl Socket for UnixStreamSocket {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        let mut inner = self.inner.lock();
        match &mut *inner {
            Inner::Init(init) => {
                let unix_ep = match endpoint {
                    Endpoint::Unix(ep) => ep,
                    _ => return Err(SyscallErr::EINVAL),
                };
                match unix_ep {
                    crate::net::socket::unix::UnixEndpoint::Unnamed => {
                        init.addr = Some(UnixEndpointBound::Unnamed);
                        Ok(0)
                    }
                    UnixEndpoint::Abstract(name) => {
                        if init.addr.is_some() {
                            return Err(SyscallErr::EINVAL); // 已绑定过地址
                        }
                        if name.is_empty() || name.len() > UNIX_PATH_MAX - 1 {
                            return Err(SyscallErr::EINVAL);
                        }
                        init.addr = Some(UnixEndpointBound::Abstract(name.clone()));
                        Ok(0)
                    }
                    UnixEndpoint::Path(ref path) => {
                        if init.addr.is_some() {
                            return Err(SyscallErr::EINVAL); // 已绑定过地址
                        }
                        init.addr = Some(UnixEndpointBound::Path(path.clone()));
                        Ok(0)
                    }
                }
            }
            Inner::Connected(conn) => {
                // 已连接的 socket 也可以 bind（设置 local_addr）
                let unix_ep = match endpoint {
                    Endpoint::Unix(ep) => ep,
                    _ => return Err(SyscallErr::EINVAL),
                };
                match unix_ep {
                    UnixEndpoint::Unnamed => {
                        conn.addr = Some(UnixEndpointBound::Unnamed);
                        Ok(0)
                    }

                    _ => Err(SyscallErr::EOPNOTSUPP),
                }
            }
            Inner::Listener(_) => Err(SyscallErr::EOPNOTSUPP),
        }
    }

    fn listen(&self) -> SyscallRet {
        let mut inner = self.inner.lock();
        let tmp = core::mem::replace(&mut *inner, Inner::Init(Init::new()));
        match tmp {
            Inner::Init(init) => {
                let addr = init.addr.clone().ok_or(SyscallErr::EINVAL)?; // 必须先 bind
                let backlog = 16; // 默认 backlog
                let listener = Listener::new(addr, backlog);
                *inner = Inner::Listener(listener);
                Ok(0)
            }
            other => {
                *inner = other;
                Err(SyscallErr::EOPNOTSUPP)
            }
        }
    }

    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        let unix_ep = match endpoint {
            Endpoint::Unix(ep) => ep,
            _ => return Err(SyscallErr::EAFNOSUPPORT),
        };
        match unix_ep {
            UnixEndpoint::Abstract(name) => {
                if name.is_empty() || name.len() > UNIX_PATH_MAX - 1 {
                    return Err(SyscallErr::EINVAL);
                }

                // 1. 从抽象表找到监听 socket
                let server_socket = ABSTRACT_TABLE
                    .lookup_abstract_name_bytes(name)
                    .ok_or(SyscallErr::ECONNREFUSED)?;

                // 2. 创建一对 Connected（client_conn 给本端，server_conn 给 listener）
                let server_recv_waiters = Arc::new(EventWaitQueue::new());
                let server_send_waiters = Arc::new(EventWaitQueue::new());
                let (mut client_conn, mut server_conn) = Connected::new_pair(
                    UNIX_STREAM_DEFAULT_BUF_SIZE,
                    self.recv_waiters.clone(),
                    self.send_waiters.clone(),
                    server_recv_waiters,
                    server_send_waiters,
                );

                // 3. 设置对端地址和凭证
                let peer_creds = crate::task::current_task().map(|t| {
                    let inner = t.acquire_inner_lock();
                    (t.pid() as u32, inner.uid, inner.gid)
                });
                client_conn.peer_addr = Some(UnixEndpointBound::Abstract(name.clone()));
                client_conn.peer_creds = peer_creds;
                server_conn.peer_creds = peer_creds;

                // 4. 通过 trait 方法把 server_conn 推入 listener 队列
                //    不再需要直接访问 server_socket.inner（dyn Socket 上访问不到）
                server_socket.push_pending_connected(server_conn)?;

                // 5. 唤醒 acceptor
                if let Some(wq) = server_socket.accept_event_queue() {
                    wq.notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
                }

                // 6. 本端变为 Connected
                let mut self_inner = self.inner.lock();
                match &mut *self_inner {
                    Inner::Init(init) => {
                        client_conn.addr = init.addr.take(); // ★ take 不 move
                        *self_inner = Inner::Connected(client_conn);
                        Ok(0)
                    }
                    _ => Err(SyscallErr::EISCONN),
                }
            }

            UnixEndpoint::Path(path) => {
                let server_socket = PATH_TABLE
                    .lock()
                    .get(path)
                    .and_then(|w| w.upgrade())
                    .ok_or(SyscallErr::ECONNREFUSED)?;

                let server_recv_waiters = Arc::new(EventWaitQueue::new());
                let server_send_waiters = Arc::new(EventWaitQueue::new());
                let (mut client_conn, mut server_conn) = Connected::new_pair(
                    UNIX_STREAM_DEFAULT_BUF_SIZE,
                    self.recv_waiters.clone(),
                    self.send_waiters.clone(),
                    server_recv_waiters,
                    server_send_waiters,
                );
                let peer_creds = crate::task::current_task().map(|t| {
                    let inner = t.acquire_inner_lock();
                    (t.pid() as u32, inner.uid, inner.gid)
                });
                client_conn.peer_addr = Some(UnixEndpointBound::Path(path.clone()));
                client_conn.peer_creds = peer_creds;
                server_conn.peer_creds = peer_creds;
                server_socket.push_pending_connected(server_conn)?;
                if let Some(wq) = server_socket.accept_event_queue() {
                    wq.notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
                }

                let mut self_inner = self.inner.lock();
                match &mut *self_inner {
                    Inner::Init(init) => {
                        client_conn.addr = init.addr.take();
                        *self_inner = Inner::Connected(client_conn);
                        Ok(0)
                    }
                    _ => Err(SyscallErr::EISCONN),
                }
            }
            _ => Err(SyscallErr::EOPNOTSUPP),
        }
    }

    fn try_connect(&self) -> Result<isize, SyscallErr> {
        // Unix 域连接是即刻完成的（无握手），所以 try_connect 始终成功
        // 但当前尚未实现 connect 逻辑，先返回 EOPNOTSUPP
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn accept(&self, _sockfd: u32, addr: usize, addrlen: usize) -> SyscallRet {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Listener(listener) => {
                let conn = listener.pop_incoming().ok_or(SyscallErr::EAGAIN)?;

                // 在对端地址（在包装前取出，因为 conn 即将被 move）
                let peer_addr = conn.peer_endpoint();

                // 把 Connected 包成 UnixStreamSocket（← 现场造，不再提前造）
                let server_socket = UnixStreamSocket::new_connected(conn, false);
                let socket: Arc<dyn Socket> = Arc::new(server_socket);
                let socket_file: Arc<dyn crate::fs::vfs::IndexNode> =
                    Arc::new(crate::net::SocketFile::new(socket));
                let vf = vfs::File::new_without_open(
                    socket_file,
                    FileFlags::O_RDWR,
                    vfs::FileType::Socket,
                );

                let task = crate::task::current_task().ok_or(SyscallErr::ESRCH)?;
                let files_ref = task.process.files();
                let fd = files_ref
                    .lock()
                    .alloc_fd(vf, false)
                    .map_err(|_| SyscallErr::ENFILE)?;

                // 填充对端地址
                if addr != 0 && addrlen >= 2 {
                    if let Some(ep) = peer_addr {
                        let _ = ep.fill_sockaddr(addr, addrlen);
                    }
                }

                Ok(fd)
            }
            _ => Err(SyscallErr::EOPNOTSUPP),
        }
    }

    fn socket_type(&self) -> PSOCK {
        PSOCK::Stream
    }

    fn recv_buf_size(&self) -> usize {
        self.recv_buf_size.load(Ordering::Relaxed)
    }

    fn send_buf_size(&self) -> usize {
        self.send_buf_size.load(Ordering::Relaxed)
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.recv_buf_size.store(size, Ordering::Relaxed);
    }

    fn set_send_buf_size(&self, size: usize) {
        self.send_buf_size.store(size, Ordering::Relaxed);
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Init(init) => init.addr.as_ref().map(|a| Endpoint::Unix(a.clone().into())),
            Inner::Connected(conn) => conn.local_endpoint(),
            Inner::Listener(listener) => Some(listener.endpoint()),
        }
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => conn.peer_endpoint(),
            _ => None,
        }
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        let inner = self.inner.lock();
        if let Inner::Connected(conn) = &*inner {
            match how {
                SHUT_RD => {
                    conn.rx.lock().set_recv_shutdown();
                }
                SHUT_WR => {
                    conn.peer_rx.lock().set_send_shutdown();
                }
                SHUT_RDWR => {
                    conn.rx.lock().set_recv_shutdown();
                    conn.peer_rx.lock().set_send_shutdown();
                }
                _ => return Err(SyscallErr::EINVAL),
            }
            self.wake_wait_queues();
            Ok(())
        } else {
            Err(SyscallErr::ENOTCONN)
        }
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => match conn.try_recv(buf) {
                Some(n) => Ok(n as isize),
                None => {
                    // 检查对端是否已关闭写入
                    if conn.rx.lock().is_send_shutdown() {
                        Ok(0) // EOF
                    } else {
                        Err(SyscallErr::EAGAIN)
                    }
                }
            },
            _ => Err(SyscallErr::ENOTCONN),
        }
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => match conn.try_send(buf) {
                Some(n) => Ok(n as isize),
                None => {
                    // 检查对端是否已关闭读取
                    if conn.peer_rx.lock().is_recv_shutdown() {
                        Err(SyscallErr::EPIPE)
                    } else {
                        Err(SyscallErr::EAGAIN)
                    }
                }
            },
            _ => Err(SyscallErr::ENOTCONN),
        }
    }

    fn socket_r_ready(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Listener(listener) => {
                !listener.incoming.lock().is_empty() // 有等待 accept 的连接
            }
            Inner::Connected(conn) => conn.recv_ready(),
            Inner::Init(_) => false,
        }
    }

    fn socket_w_ready(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => conn.send_ready(),
            _ => true, // 未连接时始终可写
        }
    }

    fn peer_creds(&self) -> Result<(u32, u32, u32), SyscallErr> {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => conn.peer_creds.ok_or(SyscallErr::ENOTCONN),
            _ => Err(SyscallErr::ENOTCONN),
        }
    }

    fn socket_hang_up(&self) -> bool {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Connected(conn) => {
                // 对端关闭了写入 = 本端 hangup
                conn.rx.lock().is_send_shutdown()
            }
            _ => false,
        }
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

    fn recv_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }

    fn accept_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn connect_ready(&self) -> bool {
        // Unix stream 连接即刻完成
        let inner = self.inner.lock();
        matches!(&*inner, Inner::Connected(_))
    }

    fn push_pending_connected(
        &self,
        conn: crate::net::socket::unix::stream::inner::Connected,
    ) -> SyscallRet {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Listener(listener) => {
                if listener.incoming.lock().len() >= listener.backlog {
                    return Err(SyscallErr::EAGAIN);
                }
                listener.push_incoming(conn);
                Ok(0)
            }
            _ => Err(SyscallErr::EOPNOTSUPP),
        }
    }
}

impl Drop for UnixStreamSocket {
    fn drop(&mut self) {
        let peer = {
            let inner = self.inner.lock();
            match &*inner {
                Inner::Connected(conn) => Some((
                    conn.peer_rx.clone(),
                    conn.rx.clone(),
                    conn.peer_recv_waiters.clone(),
                    conn.peer_send_waiters.clone(),
                )),
                Inner::Init(_) | Inner::Listener(_) => None,
            }
        };
        if let Some((peer_rx, rx, peer_recv_waiters, peer_send_waiters)) = peer {
            peer_rx.lock().set_send_shutdown();
            rx.lock().set_recv_shutdown();
            peer_recv_waiters.notify_events_all(
                EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM | EPollEvent::EPOLLHUP,
            );
            peer_send_waiters.notify_events_all(
                EPollEvent::EPOLLOUT
                    | EPollEvent::EPOLLWRNORM
                    | EPollEvent::EPOLLERR
                    | EPollEvent::EPOLLHUP,
            );
        }
        let (abstract_name, path_name) = {
            let abstract_name = {
                let inner = self.inner.lock();
                match &*inner {
                    Inner::Init(init) => init.addr.as_ref().and_then(|a| {
                        if let UnixEndpointBound::Abstract(name) = a {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }),
                    Inner::Connected(conn) => conn.addr.as_ref().and_then(|a| {
                        if let UnixEndpointBound::Abstract(name) = a {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }),
                    Inner::Listener(listener) => {
                        if let UnixEndpointBound::Abstract(name) = &listener.local_addr {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }
                }
            };
            let path_name = {
                let inner = self.inner.lock();
                match &*inner {
                    Inner::Init(init) => init.addr.as_ref().and_then(|a| {
                        if let UnixEndpointBound::Path(name) = a {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }),
                    Inner::Connected(conn) => conn.addr.as_ref().and_then(|a| {
                        if let UnixEndpointBound::Path(name) = a {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }),
                    Inner::Listener(listener) => {
                        if let UnixEndpointBound::Path(name) = &listener.local_addr {
                            Some(name.clone())
                        } else {
                            None
                        }
                    }
                }
            };
            (abstract_name, path_name)
        };

        if let Some(name) = abstract_name {
            ABSTRACT_TABLE.remove_abstract_name_bytes(&name);
        }
        if let Some(name) = path_name {
            PATH_TABLE.lock().remove(&name);
        }
    }
}
