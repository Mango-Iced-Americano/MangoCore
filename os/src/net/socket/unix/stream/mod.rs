//! Unix Stream Socket 实现
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/stream/mod.rs` 设计。
//! 当前为骨架阶段，所有方法已签名但核心逻辑用 `todo!()` 占位。

pub mod inner;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use self::inner::Connected;
use crate::net::socket::unix::UnixEndpointBound;
use crate::net::syscall::common::MsgFlags;
use crate::net::{Endpoint, Socket, PSOCK};
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};

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
    pub recv_waiters: Mutex<WaitQueue>,
    pub send_waiters: Mutex<WaitQueue>,
    pub connect_waiters: Mutex<WaitQueue>,
    pub accept_waiters: Mutex<WaitQueue>,
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
    /// 创建一个新 Unix Stream Socket（初始状态为 Init）
    pub fn new(is_nonblock: bool) -> Self {
        Self {
            inner: Mutex::new(Inner::Init(Init::new())),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            send_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        }
    }

    /// 从已有 Connected 状态创建（用于 socketpair）
    pub fn new_connected(connected: Connected, is_nonblock: bool) -> Self {
        Self {
            inner: Mutex::new(Inner::Connected(connected)),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            send_buf_size: AtomicUsize::new(UNIX_STREAM_DEFAULT_BUF_SIZE),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        }
    }

    fn is_nonblocking(&self) -> bool {
        self.is_nonblock.load(Ordering::Relaxed)
    }

    /// 唤醒所有等待队列
    pub fn wake_wait_queues(&self) {
        self.recv_waiters.lock().wake_all();
        self.send_waiters.lock().wake_all();
        self.connect_waiters.lock().wake_all();
        self.accept_waiters.lock().wake_all();
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
                // 当前仅支持 unnamed bind；文件系统路径和抽象路径尚未实现
                match unix_ep {
                    crate::net::socket::unix::UnixEndpoint::Unnamed => {
                        init.addr = Some(UnixEndpointBound::Unnamed);
                        Ok(0)
                    }
                    _ => {
                        // TODO: 实现文件系统路径和抽象命名空间 bind
                        Err(SyscallErr::EOPNOTSUPP)
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
                    crate::net::socket::unix::UnixEndpoint::Unnamed => {
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
        let _unix_ep = match endpoint {
            Endpoint::Unix(ep) => ep,
            _ => return Err(SyscallErr::EAFNOSUPPORT),
        };
        // TODO: 实现通过 backlog 表查找监听 socket 并建立连接
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn try_connect(&self) -> Result<isize, SyscallErr> {
        // Unix 域连接是即刻完成的（无握手），所以 try_connect 始终成功
        // 但当前尚未实现 connect 逻辑，先返回 EOPNOTSUPP
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        let inner = self.inner.lock();
        match &*inner {
            Inner::Listener(listener) => {
                let conn = listener.pop_incoming().ok_or(SyscallErr::EAGAIN)?;
                // TODO: 将 conn 包装为 SocketFile 并分配 fd，同时填充 addr/addrlen
                let _ = conn;
                Err(SyscallErr::EOPNOTSUPP)
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

    fn shutdown(&self, _how: u32) -> GeneralRet<()> {
        // TODO: 根据 how (SHUT_RD/SHUT_WR/SHUT_RDWR) 设置对应的 shutdown 标志
        Err(SyscallErr::EOPNOTSUPP)
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
}
