//! Unix 域数据报 Socket 实现
//!
//! 参照 DragonOS `kernel/src/net/socket/unix/datagram/mod.rs` 设计。
//! 当前为骨架阶段，核心逻辑用 `todo!()` 占位。

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::net::socket::unix::{UnixEndpoint, UnixEndpointBound};
use crate::net::{Endpoint, PSOCK, Socket};
use crate::net::syscall::common::MsgFlags;
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};

// ── 常量 ─────────────────────────────────────────────────────────────

/// 默认接收队列容量（消息条数）
const DEFAULT_RECV_QUEUE_CAPACITY: usize = 128;
/// 默认缓冲区大小
const DEFAULT_BUF_SIZE: usize = 64 * 1024;

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
    pub recv_waiters: Mutex<WaitQueue>,
    pub send_waiters: Mutex<WaitQueue>,
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
        Arc::new(Self {
            inner: Mutex::new(Inner::new()),
            is_nonblock: AtomicBool::new(is_nonblock),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
        })
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
        // 当前仅支持 unnamed bind
        match unix_ep {
            UnixEndpoint::Unnamed => {
                inner.local_addr = Some(UnixEndpointBound::Unnamed);
                Ok(0)
            }
            _ => {
                // TODO: 实现文件系统路径和抽象命名空间 bind
                Err(SyscallErr::EOPNOTSUPP)
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
            _ => {
                // TODO: 查找对端 socket 并建立关联
                Err(SyscallErr::EOPNOTSUPP)
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
        let mut inner = self.inner.lock();
        let (peer_addr, data) = {
            let peer = inner.peer_addr.clone().ok_or(SyscallErr::ENOTCONN)?;
            (peer, buf.to_vec())
        };
        // TODO: 查找 peer_addr 对应的对端 socket 并推入其 recv_queue
        let _ = (peer_addr, data);
        Err(SyscallErr::EOPNOTSUPP)
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
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.send_waiters)
    }

    fn recv_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }
}
