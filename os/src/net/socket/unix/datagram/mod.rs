//! Unix 域数据报 Socket 实现
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/datagram/mod.rs` 设计。
//!
//! # Implementation Status
//!
//! `bind`（Path/Abstract/Unnamed）、`connect`、`send_to_bound`（通过 `BIND_TABLE`
//! 查找对端并推入 `recv_queue`）、`try_recv`/`try_recvmsg`（含源地址）、
//! `try_send`/`try_sendmsg`、epoll 事件通知已实现。
//!
//! # Limitations
//!
//! - `SO_RCVBUF`/`SO_SNDBUF` 未生效（`set_recv_buf_size` / `set_send_buf_size` 空实现）
//! - `shutdown` 未实现
//! - `listen`/`accept` 不支持（数据报类型无连接语义）

use crate::net::socket::unix::{UnixEndpoint, UnixEndpointBound};
use crate::net::syscall::common::MsgFlags;
use crate::net::{Endpoint, Socket, PSOCK};
use crate::fs::vfs::event::{EPollEvent, EventWaitQueue};
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::sync::Weak;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

// ── 常量 ─────────────────────────────────────────────────────────────

/// 默认接收队列容量（消息条数）
const DEFAULT_RECV_QUEUE_CAPACITY: usize = 128;
/// 默认缓冲区大小
const DEFAULT_BUF_SIZE: usize = 64 * 1024;

// ── AbstractTable ───────────────────────────────────────────

struct AbstractTable {
    inner: Mutex<BTreeMap<Arc<[u8]>, Weak<UnixDatagramSocket>>>,
    next_abstract_id: AtomicUsize,
}

impl AbstractTable {
    fn new() -> Self {
        Self {
            inner: Mutex::new(BTreeMap::new()),
            next_abstract_id: AtomicUsize::new(0),
        }
    }

    fn insert(&self, name: Arc<[u8]>, socket: Weak<UnixDatagramSocket>) {
        self.inner.lock().insert(name, socket);
    }

    fn remove(&self, name: &[u8]) {
        self.inner.lock().remove(name);
    }

    fn get(&self, name: &[u8]) -> Option<Arc<UnixDatagramSocket>> {
        self.inner
            .lock()
            .get(&Arc::from(name))
            .and_then(|w| w.upgrade())
    }

    fn allocate_abstract_name(&self) -> Arc<[u8]> {
        let id = self.next_abstract_id.fetch_add(1, Ordering::Relaxed);
        Arc::from(format!("__abstract_{}", id).into_bytes())
    }
}

struct BindTable {
    // path
    path_table: Mutex<BTreeMap<String, Weak<UnixDatagramSocket>>>,

    // abstract
    abstract_table: AbstractTable,
}

impl BindTable {
    pub fn new() -> Self {
        Self {
            path_table: Mutex::new(BTreeMap::new()),
            abstract_table: AbstractTable::new(),
        }
    }

    pub fn register(&self, addr: &UnixEndpointBound, socket: &Arc<UnixDatagramSocket>) {
        match addr {
            UnixEndpointBound::Path(path) => {
                self.path_table
                    .lock()
                    .insert(path.clone(), Arc::downgrade(socket));
            }
            UnixEndpointBound::Abstract(name) => {
                self.abstract_table
                    .insert(Arc::from(name.as_slice()), Arc::downgrade(socket));
            }
            _ => {}
        }
    }
    pub fn unregister(&self, addr: &UnixEndpointBound) {
        match addr {
            UnixEndpointBound::Path(path) => {
                self.path_table.lock().remove(path);
            }
            UnixEndpointBound::Abstract(name) => {
                self.abstract_table.remove(name.as_slice());
            }
            _ => {}
        }
    }

    pub fn lookup(&self, addr: &UnixEndpointBound) -> Option<Arc<UnixDatagramSocket>> {
        match addr {
            UnixEndpointBound::Path(path) => {
                self.path_table.lock().get(path).and_then(|w| w.upgrade())
            }
            UnixEndpointBound::Abstract(name) => self.abstract_table.get(name.as_slice()),
            _ => None,
        }
    }

    pub fn allocate_abstract_name(&self) -> Arc<[u8]> {
        self.abstract_table.allocate_abstract_name()
    }
}

lazy_static::lazy_static! {
    static ref BIND_TABLE: BindTable = BindTable::new();
}

// ── DatagramMessage ──────────────────────────────────────────────────

/// Unix 域数据报消息
#[derive(Debug, Clone)]
struct DatagramMessage {
    /// 消息数据
    data: Vec<u8>,
    /// 发送端地址（可选）
    src_addr: Option<UnixEndpointBound>,
}

// ── Inner ────────────────────────────────────────────────────────────

/// Unix 域数据报 Socket 的内部状态
#[derive(Debug)]
struct Inner {
    /// 本地绑定地址
    local_addr: Option<UnixEndpointBound>,
    /// 连接的对端地址（用于 connect 后的 send）
    peer_addr: Option<UnixEndpointBound>,
    /// 接收队列 - 保存接收到的数据报
    recv_queue: VecDeque<DatagramMessage>,
    /// 接收队列的最大容量（消息数量）
    recv_queue_capacity: usize,
}

impl Inner {
    fn new() -> Self {
        Self {
            local_addr: None,
            peer_addr: None,
            recv_queue: VecDeque::new(),
            recv_queue_capacity: DEFAULT_RECV_QUEUE_CAPACITY,
        }
    }

    fn try_recv(&mut self, buf: &mut [u8]) -> Option<isize> {
        let msg = self.recv_queue.pop_front()?;
        let n = buf.len().min(msg.data.len());
        buf[..n].copy_from_slice(&msg.data[..n]);
        Some(n as isize)
    }

    fn try_send(&mut self, data: &[u8]) -> Option<()> {
        if self.recv_queue.len() >= self.recv_queue_capacity {
            return None;
        }
        // TODO(unix-dgram-send): `Inner::try_send` 已废弃——实际发送路径是
        // `UnixDatagramSocket::send_to_bound()`（通过 `BIND_TABLE` 查找对端并
        // 将消息推入对端 `recv_queue`）。此方法应删除或重构为 `Inner` 级别的
        // send buffer 管理。
        // Exit condition: 所有发送路径使用 `send_to_bound`（`try_send` /
        // `try_sendmsg` 均已委托），`Inner::try_send` 不再存在。
        todo!("implement peer lookup for datagram send")
    }

    fn recv_ready(&self) -> bool {
        !self.recv_queue.is_empty()
    }

    fn send_ready(&self) -> bool {
        self.recv_queue.len() < self.recv_queue_capacity
    }
}

// ── UnixDatagramSocket ───────────────────────────────────────────────

/// Unix 域数据报 Socket
pub struct UnixDatagramSocket {
    inner: Mutex<Inner>,
    is_nonblock: AtomicBool,
    pub recv_waiters: EventWaitQueue,
    pub send_waiters: EventWaitQueue,
    /// 指向自身的弱引用，用于 bind 时注册到全局绑定表
    self_ref: Mutex<Option<Weak<UnixDatagramSocket>>>,
}

impl core::fmt::Debug for UnixDatagramSocket {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnixDatagramSocket")
            .field("inner", &self.inner)
            .field("is_nonblock", &self.is_nonblock)
            .finish()
    }
}

impl UnixDatagramSocket {
    pub fn new(is_nonblock: bool) -> Arc<Self> {
        let socket = Arc::new(Self {
            inner: Mutex::new(Inner::new()),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
            self_ref: Mutex::new(None),
        });
        // 保存自身的弱引用，bind() 时可通过它升级出 Arc
        *socket.self_ref.lock() = Some(Arc::downgrade(&socket));
        socket
    }

    pub fn new_pair(is_nonblock: bool) -> (Arc<Self>, Arc<Self>) {
        let socket_a = Self::new(is_nonblock);
        let socket_b = Self::new(is_nonblock);

        let addr_a = BIND_TABLE.allocate_abstract_name().to_vec();
        let addr_b = BIND_TABLE.allocate_abstract_name().to_vec();
        BIND_TABLE.register(&UnixEndpointBound::Abstract(addr_a.clone()), &socket_a);
        BIND_TABLE.register(&UnixEndpointBound::Abstract(addr_b.clone()), &socket_b);

        {
            let mut inner_a = socket_a.inner.lock();
            let mut inner_b = socket_b.inner.lock();
            inner_a.peer_addr = Some(UnixEndpointBound::Abstract(addr_b.clone()));
            inner_b.peer_addr = Some(UnixEndpointBound::Abstract(addr_a.clone()));
        }

        (socket_a, socket_b)
    }

    fn send_to_bound(
        &self,
        peer_addr: UnixEndpointBound,
        buf: &[u8],
    ) -> Result<isize, SyscallErr> {
        let local_addr = self.inner.lock().local_addr.clone();
        let peer_socket = BIND_TABLE
            .lookup(&peer_addr)
            .ok_or(SyscallErr::ECONNREFUSED)?;

        let mut peer_inner = peer_socket.inner.lock();
        if peer_inner.recv_queue.len() >= peer_inner.recv_queue_capacity {
            return Err(SyscallErr::EAGAIN);
        }
        peer_inner.recv_queue.push_back(DatagramMessage {
            data: buf.to_vec(),
            src_addr: local_addr,
        });
        peer_socket
            .recv_waiters
            .notify_events_all(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM);
        Ok(buf.len() as isize)
    }
}

impl Socket for UnixDatagramSocket {
    /// 将 Unix 域数据报 socket 绑定到本地地址。
    ///
    /// # Semantics
    ///
    /// 解析 `UnixEndpoint` 变体：
    /// - `Unnamed`：自动分配地址，存储到 `self.inner.local_addr`。
    /// - `Abstract`/`Path`：通过 `self.self_ref` 获取 `Arc<Self>`，注册到
    ///   全局 `BIND_TABLE`（`AbstractTable` 或 `path_table`），供其他 socket
    ///   的 `send_to_bound()` 查找。
    ///
    /// 重复绑定的 socket 返回 `EINVAL`。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁和 `self.self_ref` 锁。`BIND_TABLE.register()` 获取
    /// 内部互斥锁（独立于 socket 自身锁）。
    ///
    /// # Errors
    ///
    /// - `EAFNOSUPPORT`：`endpoint` 不是 `Endpoint::Unix`
    /// - `EINVAL`：socket 已经绑定
    /// - `EIO`：无法从 `self_ref` 上取 `Weak` 引用（理论上不会发生）
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        let unix_ep = match endpoint {
            Endpoint::Unix(ep) => ep,
            _ => return Err(SyscallErr::EAFNOSUPPORT),
        };
        let mut inner = self.inner.lock();
        if inner.local_addr.is_some() {
            return Err(SyscallErr::EINVAL);
        }

        match unix_ep {
            UnixEndpoint::Unnamed => {
                inner.local_addr = Some(UnixEndpointBound::Unnamed);
                Ok(0)
            }
            UnixEndpoint::Abstract(name) => {
                let bound = UnixEndpointBound::Abstract(name.clone());
                // 从 self_ref 中取出 Weak 并升级为 Arc
                let self_arc = self
                    .self_ref
                    .lock()
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .ok_or(SyscallErr::EIO)?;
                BIND_TABLE.register(&bound, &self_arc);
                inner.local_addr = Some(bound);
                Ok(0)
            }
            UnixEndpoint::Path(path) => {
                let bound = UnixEndpointBound::Path(path.clone());
                let self_arc = self
                    .self_ref
                    .lock()
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .ok_or(SyscallErr::EIO)?;
                BIND_TABLE.register(&bound, &self_arc);
                inner.local_addr = Some(bound);
                Ok(0)
            }
        }
    }

    fn listen(&self) -> SyscallRet {
        // Datagram socket 不支持 listen
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 设置 Unix 域数据报 socket 的远程目标地址。
    ///
    /// # Semantics
    ///
    /// 存储 `peer_addr` 到 `self.inner`。对 `Abstract`/`Path` 变体，先通过
    /// `BIND_TABLE.lookup()` 验证目标 socket 已注册，否则返回 `ECONNREFUSED`。
    /// `Unnamed` 连接不需要验证。
    ///
    /// # Locking
    ///
    /// 获取 `self.inner` 锁。`BIND_TABLE.lookup()` 获取内部互斥锁。
    ///
    /// # Errors
    ///
    /// - `EAFNOSUPPORT`：`endpoint` 不是 `Endpoint::Unix`
    /// - `ECONNREFUSED`：目标地址不存在于绑定表中
    fn connect(&self, endpoint: &Endpoint) -> SyscallRet {
        let unix_ep = match endpoint {
            Endpoint::Unix(ep) => ep,
            _ => return Err(SyscallErr::EAFNOSUPPORT),
        };
        let mut inner = self.inner.lock();
        match unix_ep {
            UnixEndpoint::Unnamed => {
                inner.peer_addr = Some(UnixEndpointBound::Unnamed);
                Ok(0)
            }
            UnixEndpoint::Abstract(name) => {
                let bound = UnixEndpointBound::Abstract(name.clone());
                if let Some(peer_socket) = BIND_TABLE.lookup(&bound) {
                    inner.peer_addr = Some(bound);
                    Ok(0)
                } else {
                    // 对端地址不存在
                    Err(SyscallErr::ECONNREFUSED)
                }
            }
            UnixEndpoint::Path(path) => {
                let bound = UnixEndpointBound::Path(path.clone());
                if let Some(peer_socket) = BIND_TABLE.lookup(&bound) {
                    inner.peer_addr = Some(bound);
                    Ok(0)
                } else {
                    // 对端地址不存在
                    Err(SyscallErr::ECONNREFUSED)
                }
            }
        }
    }

    fn try_connect(&self) -> Result<isize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        // Datagram socket 不支持 accept
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn socket_type(&self) -> PSOCK {
        PSOCK::Datagram
    }

    fn recv_buf_size(&self) -> usize {
        DEFAULT_BUF_SIZE
    }

    fn send_buf_size(&self) -> usize {
        DEFAULT_BUF_SIZE
    }

    fn set_recv_buf_size(&self, _size: usize) {
        // TODO(unix-datagram-buf): 实现 `Inner` 级别的缓冲区大小调整，包括动态重分配
        // `recv_queue_capacity` 和底层 `DEFAULT_BUF_SIZE` 的联动。
        // Exit condition: `sys_setsockopt(SO_RCVBUF)` 对此 socket 类型生效，
        // 且 `sys_getsockopt(SO_RCVBUF)` 返回设置后的值。
    }

    fn set_send_buf_size(&self, _size: usize) {
        // TODO(unix-datagram-buf): 实现 `Inner` 级别的发送缓冲区大小调整，
        // 包括动态重分配 `recv_queue_capacity` 和底层 `DEFAULT_BUF_SIZE` 的联动。
        // Exit condition: `sys_setsockopt(SO_SNDBUF)` 对此 socket 类型生效，
        // 且 `sys_getsockopt(SO_SNDBUF)` 返回设置后的值。
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        let inner = self.inner.lock();
        inner
            .local_addr
            .as_ref()
            .map(|addr| Endpoint::Unix(addr.clone().into()))
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        let inner = self.inner.lock();
        inner
            .peer_addr
            .as_ref()
            .map(|addr| Endpoint::Unix(addr.clone().into()))
    }

    fn shutdown(&self, _how: u32) -> GeneralRet<()> {
        // TODO(unix-datagram-shutdown): 实现 `SHUT_RD`/`SHUT_WR`/`SHUT_RDWR` 语义。
        // 当前 unix datagram 使用内存内 `recv_queue` 无连接状态机，shutdown 至少需要
        // 标记"不再接收"和"不再发送"并通知 peer。
        // Exit condition: `sys_shutdown(sockfd, SHUT_RD/SHUT_WR)` 返回 `Ok(0)`,
        // 且对方 `write()`→`EPIPE` 或后续 `sendto()`→`ECONNREFUSED`。
        Err(SyscallErr::EOPNOTSUPP)
    }

    /// 非阻塞尝试接收 Unix 域数据报（不 poll、不睡眠）。
    ///
    /// # Semantics
    ///
    /// 从 `self.inner.recv_queue` 弹出一条消息，复制最多 `buf.len()` 字节，
    /// 超出数据静默截断。
    ///
    /// **阻塞模型**：`try_xxx` 模式——仅消费已有数据，队列为空时返回 `EAGAIN`。
    /// 与 `try_recvmsg()` 的区别：不返回源地址。
    ///
    /// # Errors
    ///
    /// - `EAGAIN`：接收队列为空
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut inner = self.inner.lock();
        inner.try_recv(buf).ok_or(SyscallErr::EAGAIN)
    }

    /// 非阻塞尝试发送到已连接的对端（不 poll、不睡眠）。
    ///
    /// # Semantics
    ///
    /// 从 `self.inner.peer_addr` 获取目标地址（必须通过 `connect()` 设置，
    /// 否则 `ENOTCONN`），然后通过 `send_to_bound()` 将消息推送到对端的
    /// `recv_queue` 并通知其 `recv_waiters`。
    ///
    /// **阻塞模型**：`try_xxx` 模式——不 poll、不睡眠。对端 `recv_queue` 满时
    /// 返回 `EAGAIN`（发送方通过 `send_wait_queue` 阻塞等待）。
    ///
    /// # Errors
    ///
    /// - `ENOTCONN`：未 `connect` 即发送
    /// - `ECONNREFUSED`：peername 不再在绑定表中
    /// - `EAGAIN`：对端 `recv_queue` 已满
    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let peer_addr = self
            .inner
            .lock()
            .peer_addr
            .clone()
            .ok_or(SyscallErr::ENOTCONN)?;
        self.send_to_bound(peer_addr, buf)
    }

    /// 非阻塞尝试发送 Unix 域数据报到指定目标（不 poll、不睡眠）。
    ///
    /// # Semantics
    ///
    /// 支持显式 `dest` 参数：
    /// - `Some(Endpoint::Unix(UnixEndpoint::Path/Abstract))`：发送到该地址
    /// - `None`：回退到 `self.try_send()`（使用 `peer_addr`）
    ///
    /// 目标地址通过 `BIND_TABLE.lookup()` 查找 peer socket，然后将消息推送到
    /// 对端的 `recv_queue`。若 peer 接收队列满则返回 `EAGAIN`。
    ///
    /// # Errors
    ///
    /// - `EINVAL`：`Unnamed` 目标（无法路由）
    /// - `EAFNOSUPPORT`：非 Unix 端点
    /// - `ECONNREFUSED`：目标不在绑定表中
    /// - `EAGAIN`：对端接收队列满
    fn try_sendmsg(
        &self,
        buf: &[u8],
        dest: Option<Endpoint>,
        flags: MsgFlags,
    ) -> Result<isize, SyscallErr> {
        match dest {
            Some(Endpoint::Unix(UnixEndpoint::Path(path))) => {
                self.send_to_bound(UnixEndpointBound::Path(path), buf)
            }
            Some(Endpoint::Unix(UnixEndpoint::Abstract(name))) => {
                self.send_to_bound(UnixEndpointBound::Abstract(name), buf)
            }
            Some(Endpoint::Unix(UnixEndpoint::Unnamed)) => Err(SyscallErr::EINVAL),
            Some(_) => Err(SyscallErr::EAFNOSUPPORT),
            None => self.try_send(buf, flags),
        }
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        let mut inner = self.inner.lock();
        if let Some(msg) = inner.recv_queue.pop_front() {
            let n = buf.len().min(msg.data.len());
            buf[..n].copy_from_slice(&msg.data[..n]);
            let src_ep = msg.src_addr.map(|addr| Endpoint::Unix(addr.clone().into()));
            Ok((n as isize, src_ep))
        } else {
            Err(SyscallErr::EAGAIN)
        }
    }

    fn socket_r_ready(&self) -> bool {
        self.inner.lock().recv_ready()
    }

    fn socket_w_ready(&self) -> bool {
        self.inner.lock().send_ready()
    }

    fn socket_hang_up(&self) -> bool {
        false
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

    fn recv_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }
}

impl Drop for UnixDatagramSocket {
    fn drop(&mut self) {
        // 在 socket 被销毁时，从绑定表中移除
        if let Some(local_addr) = &self.inner.lock().local_addr {
            BIND_TABLE.unregister(local_addr);
        }
    }
}
