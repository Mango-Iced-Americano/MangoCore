//! Unix 域数据报 Socket 实现
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/datagram/mod.rs` 设计。
//! 当前为骨架阶段，核心逻辑用 `todo!()` 占位。

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
        // 发送到已 connect 的对端，直接将消息推入自己的接收队列
        // 真正的实现需要查找对端 socket 并将消息推入对端的 recv_queue
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
    /// 创建一个新 Unix 数据报 Socket
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
        // TODO: 实现缓冲区大小调整
    }

    fn set_send_buf_size(&self, _size: usize) {
        // TODO: 实现缓冲区大小调整
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
        // TODO: 实现 shutdown
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut inner = self.inner.lock();
        inner.try_recv(buf).ok_or(SyscallErr::EAGAIN)
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        let peer_addr = self
            .inner
            .lock()
            .peer_addr
            .clone()
            .ok_or(SyscallErr::ENOTCONN)?;
        self.send_to_bound(peer_addr, buf)
    }

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
