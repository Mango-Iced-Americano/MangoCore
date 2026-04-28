use super::{Mutex, Socket};
use crate::net::TCP_SOCKETS_TO_REMOVE;
use crate::{
    fs::{file_trait::File, FileDescriptor, OpenFlags},
    net::{
        address,
        config::{lookup_source_ip, NET_INTERFACE},
        MAX_BUFFER_SIZE, SHUT_WR,
    },
    task::current_task,
    utils::{
        error::{GeneralRet, SyscallErr, SyscallRet},
        random::RNG,
    },
};
use alloc::{sync::Arc, vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use log::info;
use smoltcp::{
    iface::SocketHandle,
    socket::{self, tcp},
    wire::{IpEndpoint, IpListenEndpoint},
};

use crate::fs::directory_tree::DirectoryTreeNode;
use crate::fs::dirent::Dirent;
use crate::fs::fat32::PageCache;
use crate::fs::DiskInodeType;
use crate::fs::SeekWhence;
use crate::fs::Stat;
use crate::mm::UserBuffer;
use crate::net::macros::impl_file_for_socket;
use crate::trace_event;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Weak;
use alloc::vec::Vec;

/// Convert smoltcp tcp::State to a stable u64 code for trace_event.
fn tcp_state_code(state: &tcp::State) -> u64 {
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

pub const TCP_MSS_DEFAULT: u32 = 1 << 15;
pub const TCP_MSS: u32 = if TCP_MSS_DEFAULT > MAX_BUFFER_SIZE as u32 {
    MAX_BUFFER_SIZE as u32
} else {
    TCP_MSS_DEFAULT
};
const BACKLOG_SIZE: u32 = 16;
const LISTEN_BUFFER_SIZE: usize = 2048;

use crate::task::manager::WaitQueue;

pub struct TcpSocket {
    inner: Mutex<TcpSocketInner>,
    is_listener: AtomicBool,
    is_shutdown: AtomicBool,
    handlers: Mutex<VecDeque<SocketHandle>>,
    recv_waiters: Mutex<WaitQueue>,
    send_waiters: Mutex<WaitQueue>,
    connect_waiters: Mutex<WaitQueue>,
    accept_waiters: Mutex<WaitQueue>,
}

#[allow(unused)]
struct TcpSocketInner {
    local_endpoint: IpListenEndpoint,
    remote_endpoint: Option<IpEndpoint>,
    last_state: tcp::State,
    recvbuf_size: usize,
    sendbuf_size: usize,
    reuse_addr: bool,
    // TODO: add more
}

impl Socket for TcpSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        info!("[tcp::bind] bind to: {:?}", addr);
        self.inner.lock().local_endpoint = addr;
        Ok(0)
    }

    fn listen(&self) -> SyscallRet {
        let local = self.inner.lock().local_endpoint;
        let mut queue = self.handlers.lock();

        NET_INTERFACE
            .tcp_socket(queue[0], |s| s.listen(local))
            .ok_or(SyscallErr::EAGAIN)?
            .map_err(|_| SyscallErr::EINVAL)?;

        for _ in 1..BACKLOG_SIZE {
            let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let mut new_socket = socket::tcp::Socket::new(tx_buf, rx_buf);
            new_socket
                .listen(local)
                .map_err(|_| SyscallErr::EADDRINUSE)?;
            queue.push_back(
                NET_INTERFACE
                    .add_socket(new_socket)
                    .ok_or(SyscallErr::EAGAIN)?,
            );
        }

        self.is_listener.store(true, Ordering::Release);

        Ok(0)
    }

    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> crate::utils::error::SyscallRet {
        if !self.is_listener.load(Ordering::Acquire) {
            return Err(SyscallErr::EINVAL);
        }
        // get old socket
        let task = current_task().unwrap();
        let old_nonblock = task
            .files
            .lock()
            .get_ref(sockfd as usize)
            .unwrap()
            .get_nonblock();
        let (connected_handler, peer_endpoint) = self._accept(old_nonblock)?;
        log::info!("[Socket::accept] connection established");

        let mut new_handlers = VecDeque::new();
        new_handlers.push_back(connected_handler);

        let (local_ep, rx_sz, tx_sz, reuse) = {
            let inner = self.inner.lock();
            (
                inner.local_endpoint,
                inner.recvbuf_size,
                inner.sendbuf_size,
                inner.reuse_addr,
            )
        };

        let connected_socket = Arc::new(TcpSocket {
            handlers: Mutex::new(new_handlers),
            is_listener: AtomicBool::new(false),
            is_shutdown: AtomicBool::new(false),
            inner: Mutex::new(TcpSocketInner {
                local_endpoint: local_ep,
                remote_endpoint: Some(peer_endpoint),
                last_state: tcp::State::Established,
                recvbuf_size: rx_sz,
                sendbuf_size: tx_sz,
                reuse_addr: reuse,
            }),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        });
        TcpSocket::register_tcp_socket(&connected_socket);

        let mut fd_table = task.files.lock();
        let mut socket_table = task.socket_table.lock();

        let old_cloexec = fd_table.get_ref(sockfd as usize).unwrap().get_cloexec();
        let new_fd = fd_table
            .insert(FileDescriptor::new(
                old_cloexec,
                old_nonblock,
                connected_socket.clone(),
            ))
            .map_err(|_| SyscallErr::EMFILE)?;

        socket_table.insert(new_fd, connected_socket);

        address::fill_with_endpoint(peer_endpoint, addr, addrlen)?;

        Ok(new_fd)
    }

    fn socket_type(&self) -> super::SocketType {
        super::SocketType::SOCK_STREAM
    }

    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> crate::utils::error::SyscallRet {
        if self.is_listener.load(Ordering::Acquire) {
            return Err(SyscallErr::EINVAL);
        }
        let remote_endpoint = address::endpoint(addr_buf)?;
        self._connect(remote_endpoint)?;
        // 只做一次非阻塞状态检查，不做loop
        self.try_connect().map(|_| 0)
    }

    fn try_connect(&self) -> Result<isize, SyscallErr> {
        NET_INTERFACE.poll();
        let handler = { *self.handlers.lock().front().ok_or(SyscallErr::ENOTCONN)? };
        let mut state_code = 99u64;
        let state = NET_INTERFACE
            .tcp_socket(handler, |socket| {
                let s = socket.state();
                state_code = tcp_state_code(&s);
                s
            })
            .unwrap_or(tcp::State::Closed);
        trace_event!(0xB030, handler.as_usize() as u64, state_code, 0, 0, 0, 0);
        match state {
            tcp::State::Established => {
                info!("[Tcp::try_connect] connected");
                Ok(0)
            }
            tcp::State::Closed => {
                // 连接被主动拒绝（RST）或对端无法到达，不再重试
                info!(
                    "[Tcp::try_connect] {} closed (RST), connection refused",
                    handler
                );
                Err(SyscallErr::ECONNREFUSED)
            }
            _ => {
                log::trace!(
                    "[Tcp::try_connect] {} not connect yet, state {:?}",
                    handler,
                    state
                );
                Err(SyscallErr::EAGAIN)
            }
        }
    }
    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size;
    }

    fn local_endpoint(&self) -> IpListenEndpoint {
        self.inner.lock().local_endpoint
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        if self.is_listener.load(Ordering::Acquire) {
            return None;
        }
        NET_INTERFACE.poll();
        let handler = { self.handlers.lock().front().copied()? };
        NET_INTERFACE.tcp_socket(handler, |socket| socket.remote_endpoint())?
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        info!("[TcpSocket::shutdown] how {}", how);
        let handler = { *self.handlers.lock().front().ok_or(SyscallErr::ENOTCONN)? };
        NET_INTERFACE.tcp_socket(handler, |socket| match how {
            SHUT_WR => socket.close(),
            _ => socket.abort(),
        });
        self.is_shutdown.store(true, Ordering::Release);
        NET_INTERFACE.poll();
        Ok(())
    }

    fn set_nagle_enabled(&self, enabled: bool) -> SyscallRet {
        let handles: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
        for handler in handles {
            NET_INTERFACE.tcp_socket(handler, |socket| socket.set_nagle_enabled(enabled));
        }
        Ok(0)
    }

    fn set_keep_alive(&self, enabled: bool) -> SyscallRet {
        if enabled {
            let handles: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
            for handler in handles {
                NET_INTERFACE.tcp_socket(handler, |socket| {
                    socket.set_keep_alive(Some(Duration::from_secs(7200).into()))
                });
            }
        } else {
            let handles: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
            for handler in handles {
                NET_INTERFACE.tcp_socket(handler, |socket| socket.set_keep_alive(None));
            }
        }
        Ok(0)
    }

    fn reuse_addr(&self) -> SyscallRet {
        let reuse_addr = self.inner.lock().reuse_addr;
        Ok(reuse_addr as usize)
    }

    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet {
        self.inner.lock().reuse_addr = enabled;
        Ok(0)
    }

    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet {
        todo!();
    }

    fn tcp_state(&self) -> Option<u8> {
        use tcp::State::*;
        let handler = self.handlers.lock().front().copied()?;
        let state = NET_INTERFACE.tcp_socket(handler, |s| s.state())?;
        let mapped = match state {
            Established => 1,
            SynSent => 2,
            SynReceived => 3,
            FinWait1 => 4,
            FinWait2 => 5,
            TimeWait => 6,
            Closed => 7,
            CloseWait => 8,
            LastAck => 9,
            Listen => 10,
            Closing => 11,
            _ => 7,
        };
        Some(mapped)
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        if self.is_listener.load(Ordering::Acquire) {
            return Err(SyscallErr::EINVAL);
        }
        let handler = {
            let queue = self.handlers.lock();
            *queue.front().ok_or(SyscallErr::ENOTCONN)?
        };
        let ret = NET_INTERFACE
            .tcp_socket(handler, |socket| {
                if socket.can_recv() {
                    match socket.recv_slice(buf) {
                        Ok(nbytes) => Ok(nbytes as isize),
                        Err(_) => Err(SyscallErr::ENOTCONN),
                    }
                } else if !socket.may_recv() {
                    Ok(0)
                } else {
                    Err(SyscallErr::EAGAIN)
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN));
        if let Ok(_) = ret {
            let state = NET_INTERFACE
                .tcp_socket(handler, |s| s.state())
                .unwrap_or(tcp::State::Closed);
            self.inner.lock().last_state = state;
        }
        ret
    }

    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        // 已 shutdown 的 socket 返回 EPIPE 而非 ENOTCONN
        if self.is_shutdown.load(Ordering::Acquire) {
            return Err(SyscallErr::EPIPE);
        }
        if self.is_listener.load(Ordering::Acquire) {
            return Err(SyscallErr::EINVAL);
        }
        let handler = {
            let queue = self.handlers.lock();
            match queue.front() {
                Some(&h) => h,
                None => return Err(SyscallErr::ENOTCONN),
            }
        };
        NET_INTERFACE
            .tcp_socket(handler, |socket| {
                if !socket.may_send() {
                    Err(SyscallErr::EPIPE)
                } else if !socket.can_send() {
                    Err(SyscallErr::EAGAIN)
                } else {
                    match socket.send_slice(buf) {
                        Ok(nbytes) => Ok(nbytes as isize),
                        Err(_) => Err(SyscallErr::ENOTCONN),
                    }
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<IpEndpoint>), SyscallErr> {
        self.try_recv(buf).map(|n| (n, None))
    }

    fn try_sendmsg(&self, buf: &[u8], _dest: Option<IpEndpoint>) -> Result<isize, SyscallErr> {
        // TCP 忽略 dest
        self.try_send(buf)
    }
    fn socket_r_ready(&self) -> bool {
        NET_INTERFACE.poll();
        let is_listener = self.is_listener.load(Ordering::Acquire);
        let handlers: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
        let mut ret = false;

        trace_event!(
            0xB001,
            is_listener as u64,
            handlers.len() as u64,
            0,
            0,
            0,
            0
        );

        if is_listener {
            for (idx, &handler) in handlers.iter().enumerate() {
                let mut state_code = 99u64;
                NET_INTERFACE.tcp_socket(handler, |s| {
                    let state = s.state();
                    state_code = tcp_state_code(&state);
                    if state == tcp::State::SynReceived || state == tcp::State::Established {
                        ret = true;
                    }
                });
                trace_event!(
                    0xB002,
                    idx as u64,
                    handler.as_usize() as u64,
                    state_code,
                    0,
                    0,
                    0
                );
                if ret {
                    break;
                }
            }
        } else {
            if let Some(&handler) = handlers.first() {
                let mut state_code = 99u64;
                let mut can_recv = 0u64;
                let mut may_recv = 0u64;
                let mut is_eof = 0u64;
                NET_INTERFACE.tcp_socket(handler, |socket| {
                    let state = socket.state();
                    state_code = tcp_state_code(&state);
                    can_recv = socket.can_recv() as u64;
                    may_recv = socket.may_recv() as u64;
                    let is_eof_or_error = !socket.may_recv()
                        && state != tcp::State::Listen
                        && state != tcp::State::SynSent
                        && state != tcp::State::SynReceived;
                    is_eof = is_eof_or_error as u64;
                    ret = can_recv != 0 || is_eof_or_error;
                });
                trace_event!(
                    0xB003,
                    handler.as_usize() as u64,
                    state_code,
                    can_recv,
                    may_recv,
                    is_eof,
                    ret as u64
                );
            } else {
                trace_event!(0xB001, 0xFF, 0, 0, 0, 0, 0);
            }
        }
        trace_event!(0xB004, ret as u64, 0, 0, 0, 0, 0);
        ret
    }

    fn socket_w_ready(&self) -> bool {
        NET_INTERFACE.poll();
        if self.is_listener.load(Ordering::Acquire) {
            return false;
        }
        let handler = {
            let queue = self.handlers.lock();
            match queue.front() {
                Some(&h) => h,
                None => return false,
            }
        };
        NET_INTERFACE
            .tcp_socket(handler, |socket| {
                let state = socket.state();
                if state == tcp::State::Closed {
                    true
                } else {
                    socket.can_send() && socket.may_send()
                }
            })
            .unwrap_or(false)
    }

    fn socket_hang_up(&self) -> bool {
        false
    }

    fn deep_clone_socket(&self) -> Arc<dyn File> {
        todo!()
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
}

/// Linux struct tcp_info (from /usr/include/linux/tcp.h)
/// 用于 getsockopt(TCP_INFO)，netperf 等程序通过 tcpi_state 判断连接状态。
/// 所有字段必须填充或置零，否则未初始化的内存会误导用户程序。
#[repr(C)]
pub struct TcpInfo {
    tcpi_state: u8,
    tcpi_ca_state: u8,
    tcpi_retransmits: u8,
    tcpi_probes: u8,
    tcpi_backoff: u8,
    tcpi_options: u8,
    tcpi_snd_wscale: u8,
    tcpi_rcv_wscale: u8,

    tcpi_rto: u32,
    tcpi_ato: u32,
    tcpi_snd_mss: u32,
    tcpi_rcv_mss: u32,

    tcpi_unacked: u32,
    tcpi_sacked: u32,
    tcpi_lost: u32,
    tcpi_retrans: u32,
    tcpi_fackets: u32,

    /* Times */
    tcpi_last_data_sent: u32,
    tcpi_last_ack_sent: u32,
    tcpi_last_data_recv: u32,
    tcpi_last_ack_recv: u32,

    /* Metrics */
    tcpi_pmtu: u32,
    tcpi_rcv_ssthresh: u32,
    tcpi_rtt: u32,
    tcpi_rttvar: u32,
    tcpi_snd_ssthresh: u32,
    tcpi_snd_cwnd: u32,
    tcpi_advmss: u32,
    tcpi_reordering: u32,

    tcpi_rcv_rtt: u32,
    tcpi_rcv_space: u32,

    tcpi_total_retrans: u32,

    tcpi_pacing_rate: u64,
    tcpi_max_pacing_rate: u64,
    tcpi_bytes_acked: u64,
    tcpi_bytes_received: u64,
    tcpi_segs_out: u32,
    tcpi_segs_in: u32,

    tcpi_notsent_bytes: u32,
    tcpi_min_rtt: u32,
    tcpi_data_segs_in: u32,
    tcpi_data_segs_out: u32,
    tcpi_delivery_rate: u64,

    tcpi_busy_time: u64,
    tcpi_rwnd_limited: u64,
    tcpi_sndbuf_limited: u64,

    tcpi_delivered: u32,
    tcpi_delivered_ce: u32,

    tcpi_bytes_sent: u64,
    tcpi_bytes_retrans: u64,
    tcpi_dsack_dups: u32,
    tcpi_reord_seen: u32,

    tcpi_rcv_ooopack: u32,
    tcpi_snd_wnd: u32,
}

impl TcpInfo {
    pub fn new(state: u8, mss: u32) -> Self {
        Self {
            tcpi_state: state,
            tcpi_ca_state: 0,
            tcpi_retransmits: 0,
            tcpi_probes: 0,
            tcpi_backoff: 0,
            tcpi_options: 0,
            tcpi_snd_wscale: 0,
            tcpi_rcv_wscale: 0,

            tcpi_rto: 0,
            tcpi_ato: 0,
            tcpi_snd_mss: mss,
            tcpi_rcv_mss: mss,

            tcpi_unacked: 0,
            tcpi_sacked: 0,
            tcpi_lost: 0,
            tcpi_retrans: 0,
            tcpi_fackets: 0,

            tcpi_last_data_sent: 0,
            tcpi_last_ack_sent: 0,
            tcpi_last_data_recv: 0,
            tcpi_last_ack_recv: 0,

            tcpi_pmtu: 0,
            tcpi_rcv_ssthresh: 0,
            tcpi_rtt: 0,
            tcpi_rttvar: 0,
            tcpi_snd_ssthresh: 0,
            tcpi_snd_cwnd: 0,
            tcpi_advmss: mss,
            tcpi_reordering: 0,

            tcpi_rcv_rtt: 0,
            tcpi_rcv_space: 0,

            tcpi_total_retrans: 0,

            tcpi_pacing_rate: 0,
            tcpi_max_pacing_rate: 0,
            tcpi_bytes_acked: 0,
            tcpi_bytes_received: 0,
            tcpi_segs_out: 0,
            tcpi_segs_in: 0,

            tcpi_notsent_bytes: 0,
            tcpi_min_rtt: 0,
            tcpi_data_segs_in: 0,
            tcpi_data_segs_out: 0,
            tcpi_delivery_rate: 0,

            tcpi_busy_time: 0,
            tcpi_rwnd_limited: 0,
            tcpi_sndbuf_limited: 0,

            tcpi_delivered: 0,
            tcpi_delivered_ce: 0,

            tcpi_bytes_sent: 0,
            tcpi_bytes_retrans: 0,
            tcpi_dsack_dups: 0,
            tcpi_reord_seen: 0,

            tcpi_rcv_ooopack: 0,
            tcpi_snd_wnd: 0,
        }
    }
}

impl TcpSocket {
    pub fn new() -> Self {
        let tx_buf = socket::tcp::SocketBuffer::new(vec![0 as u8; MAX_BUFFER_SIZE]);
        let rx_buf = socket::tcp::SocketBuffer::new(vec![0 as u8; MAX_BUFFER_SIZE]);
        let socket = socket::tcp::Socket::new(rx_buf, tx_buf);
        let socket_handler = NET_INTERFACE.add_socket(socket).unwrap();
        let mut handlers = VecDeque::new();
        handlers.push_back(socket_handler);
        log::info!("[TcpSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        Self {
            handlers: Mutex::new(handlers),
            is_listener: AtomicBool::new(false),
            is_shutdown: AtomicBool::new(false),
            inner: Mutex::new(TcpSocketInner {
                local_endpoint: IpListenEndpoint {
                    addr: None,
                    port: unsafe { RNG.positive_u32() as u16 },
                },
                remote_endpoint: None,
                last_state: tcp::State::Closed,
                recvbuf_size: MAX_BUFFER_SIZE,
                sendbuf_size: MAX_BUFFER_SIZE,
                reuse_addr: false,
            }),
            recv_waiters: Mutex::new(WaitQueue::new()),
            send_waiters: Mutex::new(WaitQueue::new()),
            connect_waiters: Mutex::new(WaitQueue::new()),
            accept_waiters: Mutex::new(WaitQueue::new()),
        }
    }

    /// 注册 TCP socket 到全局表，供 wake_tcp_waiters 使用
    pub fn register_tcp_socket(socket: &Arc<Self>) {
        let handler = *socket.handlers.lock().front().unwrap();
        crate::net::TCP_SOCKETS
            .lock()
            .push((handler, Arc::downgrade(socket)));
    }

    ///
    pub fn wake_if_ready(&self) {
        let is_listener = self.is_listener.load(Ordering::Acquire);
        let handlers: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };

        if is_listener {
            if self.accept_waiters.lock().is_empty() {
                return;
            }
            for handler in handlers {
                let has_new = NET_INTERFACE
                    .tcp_socket(handler, |s| s.state() == tcp::State::Established)
                    .unwrap_or(false);
                if has_new {
                    log::info!("[TcpSocket::wake_if_ready] listener {} has new connection, waking accept waiters", handler);
                    self.accept_waiters.lock().wake_all();
                    return;
                }
            }
        } else {
            // 普通 socket：recv / send / connect 各队列独立检查
            let handler = match handlers.first() {
                Some(&h) => h,
                None => return,
            };

            // recv：有数据可读 或 对端已关闭（EOF）
            if !self.recv_waiters.lock().is_empty() {
                let ready = NET_INTERFACE
                    .tcp_socket(handler, |s| s.can_recv() || !s.may_recv())
                    .unwrap_or(true);
                if ready {
                    self.recv_waiters.lock().wake_at_most(1);
                }
            }

            // send：发送窗口有空 或 连接已关闭
            if !self.send_waiters.lock().is_empty() {
                if self.is_shutdown.load(Ordering::Acquire) {
                    self.send_waiters.lock().wake_at_most(1);
                } else {
                    let ready = NET_INTERFACE
                        .tcp_socket(handler, |s| s.can_send() || s.state() == tcp::State::Closed)
                        .unwrap_or(true);
                    if ready {
                        self.send_waiters.lock().wake_at_most(1);
                    }
                }
            }

            // connect：连接建立 或 被拒绝
            if !self.connect_waiters.lock().is_empty() {
                let state = NET_INTERFACE
                    .tcp_socket(handler, |s| s.state())
                    .unwrap_or(tcp::State::Closed);
                match state {
                    tcp::State::Established | tcp::State::Closed => {
                        self.connect_waiters.lock().wake_at_most(1);
                    }
                    _ => {}
                }
            }
        }
    }

    /// 唤醒此 socket 上所有等待队列（由 wake_tcp_waiters 在 poll 后调用）
    pub fn wake_wait_queues(&self) {
        self.recv_waiters.lock().wake_all();
        self.send_waiters.lock().wake_all();
        self.connect_waiters.lock().wake_all();
        self.accept_waiters.lock().wake_all();
    }

    fn _connect(&self, remote_endpoint: IpEndpoint) -> GeneralRet<()> {
        if self.is_listener.load(Ordering::Acquire) {
            return Err(SyscallErr::EINVAL);
        }
        self.inner.lock().remote_endpoint = Some(remote_endpoint);
        let mut local = self.inner.lock().local_endpoint;
        if local.addr.is_none() {
            local.addr = Some(lookup_source_ip(remote_endpoint.addr));
        }
        info!(
            "[Tcp::connect] local: {:?}, remote: {:?}",
            local, remote_endpoint
        );
        let handler = { *self.handlers.lock().front().ok_or(SyscallErr::EBADF)? };
        NET_INTERFACE
            .inner_handler(|inner| {
                let socket = inner.sockets.get_mut::<tcp::Socket>(handler);
                let before_state = tcp_state_code(&socket.state());
                let ret = socket.connect(inner.iface.context(), remote_endpoint, local);
                let after_state = tcp_state_code(&socket.state());
                let ret_ok = ret.is_ok() as u64;
                trace_event!(
                    0xB020,
                    handler.as_usize() as u64,
                    before_state,
                    after_state,
                    ret_ok,
                    0,
                    0
                );
                if ret.is_err() {
                    log::info!("[Tcp::connect] {} connect error occur", handler);
                    match ret.err().unwrap() {
                        tcp::ConnectError::Unaddressable => return Err(SyscallErr::EINVAL),
                        tcp::ConnectError::InvalidState => return Err(SyscallErr::EISCONN),
                    }
                }
                info!("berfore poll socket state: {}", socket.state());
                Ok(())
            })
            .ok_or(SyscallErr::EAGAIN)??;
        Ok(())
    }
    fn _accept(&self, nonblock: bool) -> GeneralRet<(SocketHandle, IpEndpoint)> {
        if self.is_shutdown.load(Ordering::Acquire) {
            log::info!("[TcpSocket::_accept] socket is shutdown, cannot accept new connections");
            return Err(SyscallErr::ENOTCONN);
        }

        NET_INTERFACE.poll();

        // 先读取 local_endpoint（inner.lock），保持 inner→handlers 的锁顺序
        let local_ep = self.inner.lock().local_endpoint;

        // 在 handlers 锁的作用域内完成 查找+摘除，消除竞态条件
        let mut queue = self.handlers.lock();
        let mut found_idx = None;
        let mut peer_endpoint = None;
        trace_event!(0xB010, queue.len() as u64, 0, 0, 0, 0, 0);
        for (i, &handler) in queue.iter().enumerate() {
            let (state, remote) = NET_INTERFACE
                .tcp_socket(handler, |s| (s.state(), s.remote_endpoint()))
                .unwrap_or((tcp::State::Closed, None));
            let state_code = tcp_state_code(&state);
            trace_event!(
                0xB011,
                i as u64,
                handler.as_usize() as u64,
                state_code,
                0,
                0,
                0
            );
            if state == tcp::State::SynReceived || state == tcp::State::Established {
                found_idx = Some(i);
                peer_endpoint = remote;
                trace_event!(0xB012, i as u64, handler.as_usize() as u64, 0, 0, 0, 0);
                break;
            }
        }

        if let Some(idx) = found_idx {
            let handler_to_remove = queue.remove(idx).unwrap();
            log::info!(
                "[Socket::accept] found new connection {}, peer {:?}",
                handler_to_remove,
                peer_endpoint
            );

            //补充新的listener
            let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let mut new_socket = socket::tcp::Socket::new(rx_buf, tx_buf);
            new_socket.listen(local_ep).unwrap(); //如果unwrap失败说明底层有问题
            let new_handler = NET_INTERFACE.add_socket(new_socket).unwrap();
            queue.push_back(new_handler);
            return Ok((handler_to_remove, peer_endpoint.unwrap()));
        } else {
            // 连接建立后由上层循环调用 accept，直到 accept 成功或发生错误
            return Err(SyscallErr::EAGAIN);
        }
    }
}
impl Drop for TcpSocket {
    fn drop(&mut self) {
        // Collect handles under the lock, then release it.
        let handles: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };

        for &h in handles.iter() {
            // Now we no longer hold the handlers lock, so locking `inner` is safe.
            NET_INTERFACE.tcp_socket(h, |s| s.close());
            TCP_SOCKETS_TO_REMOVE.lock().push(h);
            self.is_shutdown.store(true, Ordering::Release);
            log::info!("[TcpSocket::drop] marked TCP socket {} to remove", h);
        }
    }
}

impl_file_for_socket!(TcpSocket);
