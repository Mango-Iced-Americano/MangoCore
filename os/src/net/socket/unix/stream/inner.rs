//! Unix Stream Socket 内部状态机
//!
//! 参照 DragonOS `net/socket/unix/stream/inner.rs` 设计，但大幅简化：
//! - 使用 `RingBuffer<u8>` 替代 DragonOS 的原子 RingBuffer
//! - Connected 状态下两个方向各有独立的 RingBuffer
//! - 没有 SCM_RIGHTS / SCM_CREDENTIALS 控制消息支持
//! - 没有 SO_SNDBUF / SO_RCVBUF 动态调整
//!
//! 状态机：
//!   Init      — 刚创建、已 bind
//!   Connected — 已连接（双向通信）
//!   Listener  — 正在 listen

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use spin::Mutex;

use crate::net::socket::unix::ring_buffer::RingBuffer;
use crate::net::socket::unix::{UnixEndpoint, UnixEndpointBound};
use crate::net::Endpoint;

/// 默认收发缓冲区大小（字节数）
pub const UNIX_STREAM_DEFAULT_BUF_SIZE: usize = 64 * 1024;

use core::sync::atomic::AtomicUsize;
static UNIX_RING_COUNT: AtomicUsize = AtomicUsize::new(0);
static UNIX_RING_BYTES: AtomicUsize = AtomicUsize::new(0);
pub fn unix_ring_alive() -> usize { UNIX_RING_COUNT.load(core::sync::atomic::Ordering::Relaxed) }
pub fn unix_ring_bytes() -> usize { UNIX_RING_BYTES.load(core::sync::atomic::Ordering::Relaxed) }

// ── Inner 状态机 ─────────────────────────────────────────────────────

#[derive(Debug)]
pub enum Inner {
    Init(Init),
    Connected(Connected),
    Listener(Listener),
}

// ── Init ─────────────────────────────────────────────────────────────

/// 初始状态：socket 已创建，尚未 connect 或 listen。
#[derive(Debug)]
pub struct Init {
    /// 本地绑定地址（可选）
    pub addr: Option<UnixEndpointBound>,
}

impl Init {
    pub fn new() -> Self {
        Self { addr: None }
    }
}

// ── Connected ────────────────────────────────────────────────────────

/// 已连接状态：两端的 socket 通过环形缓冲区交换数据。
///
/// # 双向通信设计
/// - `peer_rx`: 指向对端的接收缓冲区的 producer 端（本端→对端）
/// - `rx`: 本端的接收缓冲区的 consumer 端（对端→本端）
/// - 每个连接共用两个 RingBuffer，每个方向一个
/// - 两个 RingBuffer 由 `Arc<Mutex<>>` 共享
#[derive(Debug)]
pub struct Connected {
    /// 本端地址
    pub addr: Option<UnixEndpointBound>,
    /// 对端地址
    pub peer_addr: Option<UnixEndpointBound>,
    /// 对端进程凭证 (pid, uid, gid) — SO_PEERCRED
    pub peer_creds: Option<(u32, u32, u32)>,
    /// 发送缓冲区（写入此缓冲区 → 对端可以读到）
    pub peer_rx: Arc<Mutex<RingBuffer<u8>>>,
    /// 接收缓冲区（从此缓冲区读取 → 对端写入的数据）
    pub rx: Arc<Mutex<RingBuffer<u8>>>,
}

impl Connected {
    /// 创建一对已连接的 Connected 状态（用于 socketpair）。
    ///
    /// 返回 `(side_a, side_b)`，其中：
    /// - `side_a.peer_rx == side_b.rx`
    /// - `side_a.rx == side_b.peer_rx`
    pub fn new_pair(buf_size: usize) -> (Self, Self) {
        UNIX_RING_COUNT.fetch_add(2, core::sync::atomic::Ordering::Relaxed);
        UNIX_RING_BYTES.fetch_add(buf_size * 2, core::sync::atomic::Ordering::Relaxed);
        let buf_a = Arc::new(Mutex::new(RingBuffer::new(buf_size)));
        let buf_b = Arc::new(Mutex::new(RingBuffer::new(buf_size)));
        (
            Self {
                addr: None,
                peer_addr: None,
                peer_creds: None,
                peer_rx: buf_b.clone(),
                rx: buf_a.clone(),
            },
            Self {
                addr: None,
                peer_addr: None,
                peer_creds: None,
                peer_rx: buf_a,
                rx: buf_b,
            },
        )
    }

    /// 尝试读取数据
    pub fn try_recv(&self, buf: &mut [u8]) -> Option<usize> {
        let mut rx = self.rx.lock();
        if rx.is_empty() {
            return None;
        }
        let n = buf.len().min(rx.len());
        for i in 0..n {
            buf[i] = rx.pop().unwrap();
        }
        Some(n)
    }

    /// 尝试写入数据
    pub fn try_send(&self, buf: &[u8]) -> Option<usize> {
        let mut peer_rx = self.peer_rx.lock();
        let free = peer_rx.free_len();
        if free == 0 {
            return None;
        }
        let n = buf.len().min(free);
        for &b in buf {
            peer_rx.push(b);
        }
        Some(n)
    }

    /// 接收缓冲区是否可读
    pub fn recv_ready(&self) -> bool {
        !self.rx.lock().is_empty()
    }

    /// 发送缓冲区是否可写
    pub fn send_ready(&self) -> bool {
        self.peer_rx.lock().free_len() > 0
    }

    /// 获取本端端点
    pub fn local_endpoint(&self) -> Option<Endpoint> {
        self.addr
            .as_ref()
            .map(|addr| Endpoint::Unix(addr.clone().into()))
    }

    /// 获取对端端点
    pub fn peer_endpoint(&self) -> Option<Endpoint> {
        self.peer_addr
            .as_ref()
            .map(|addr| Endpoint::Unix(addr.clone().into()))
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        UNIX_RING_COUNT.fetch_sub(2, core::sync::atomic::Ordering::Relaxed);
        UNIX_RING_BYTES.fetch_sub(UNIX_STREAM_DEFAULT_BUF_SIZE * 2, core::sync::atomic::Ordering::Relaxed);
    }
}

// ── Listener ─────────────────────────────────────────────────────────

/// 监听状态：接受来自对端的连接请求。
#[derive(Debug)]
pub struct Listener {
    /// 监听地址
    pub local_addr: UnixEndpointBound,
    /// backlog（待 accept 的最大连接数）
    pub backlog: usize,
    /// 待 accept 的连接队列（存 Connected，accept 时现场包成 UnixStreamSocket）
    pub incoming: Mutex<VecDeque<Connected>>,
}

impl Listener {
    pub fn new(addr: UnixEndpointBound, backlog: usize) -> Self {
        Self {
            local_addr: addr,
            backlog,
            incoming: Mutex::new(VecDeque::new()),
        }
    }

    /// 添加一个待处理的连接
    pub fn push_incoming(&self, conn: Connected) {
        let mut incoming = self.incoming.lock();
        if incoming.len() < self.backlog {
            incoming.push_back(conn);
        }
    }

    /// 取出一个待处理的连接
    pub fn pop_incoming(&self) -> Option<Connected> {
        self.incoming.lock().pop_front()
    }

    /// 获取本端端点
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::Unix(self.local_addr.clone().into())
    }
}
