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
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub const TCP_MSS_DEFAULT: u32 = 1 << 15;
pub const TCP_MSS: u32 = if TCP_MSS_DEFAULT > MAX_BUFFER_SIZE as u32 {
    MAX_BUFFER_SIZE as u32
} else {
    TCP_MSS_DEFAULT
};
const BACKLOG_SIZE: u32 = 16;
const LISTEN_BUFFER_SIZE: usize = 2048;

pub struct TcpSocket {
    inner: Mutex<TcpSocketInner>,
    is_listener: AtomicBool,
    is_shutdown: AtomicBool,
    handlers: Mutex<VecDeque<SocketHandle>>,
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
            .map_err(|_| SyscallErr::EINVAL)?;

        for _ in 1..BACKLOG_SIZE {
            let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let mut new_socket = socket::tcp::Socket::new(tx_buf, rx_buf);
            new_socket
                .listen(local)
                .map_err(|_| SyscallErr::EADDRINUSE)?;
            queue.push_back(NET_INTERFACE.add_socket(new_socket));
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
        });

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
        let state = NET_INTERFACE.tcp_socket(handler, |socket| socket.state());
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

    fn loacl_endpoint(&self) -> IpListenEndpoint {
        self.inner.lock().local_endpoint
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        if self.is_listener.load(Ordering::Acquire) {
            return None;
        }
        NET_INTERFACE.poll();
        let handler = { self.handlers.lock().front().copied()? };
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| socket.remote_endpoint());
        NET_INTERFACE.poll();
        ret
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
        let state = NET_INTERFACE.tcp_socket(handler, |s| s.state());
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
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| {
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
        });
        if let Ok(_) = ret {
            let state = NET_INTERFACE.tcp_socket(handler, |s| s.state());
            self.inner.lock().last_state = state;
        }
        ret
    }

    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
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
        NET_INTERFACE.tcp_socket(handler, |socket| {
            if !socket.may_send() {
                Err(SyscallErr::ENOTCONN)
            } else if !socket.can_send() {
                Err(SyscallErr::EAGAIN)
            } else {
                match socket.send_slice(buf) {
                    Ok(nbytes) => Ok(nbytes as isize),
                    Err(_) => Err(SyscallErr::ENOTCONN),
                }
            }
        })
    }

    fn socket_r_ready(&self) -> bool {
        NET_INTERFACE.poll();
        let is_listener = self.is_listener.load(Ordering::Acquire);
        let handlers: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
        let mut ret = false;

        if is_listener {
            for handler in handlers {
                NET_INTERFACE.tcp_socket(handler, |s| {
                    if s.state() == tcp::State::SynReceived || s.state() == tcp::State::Established
                    {
                        ret = true;
                    }
                });
                if ret {
                    break;
                }
            }
        } else {
            if let Some(&handler) = handlers.first() {
                NET_INTERFACE.tcp_socket(handler, |socket| {
                    let can_recv = socket.can_recv();
                    let is_eof_or_error = !socket.may_recv()
                        && socket.state() != tcp::State::Listen
                        && socket.state() != tcp::State::SynSent
                        && socket.state() != tcp::State::SynReceived;
                    ret = can_recv || is_eof_or_error;
                });
            }
        }
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
        NET_INTERFACE.tcp_socket(handler, |socket| {
            let state = socket.state();
            if state == tcp::State::Closed {
                true
            } else {
                socket.can_send() && socket.may_send()
            }
        })
    }

    fn socket_hang_up(&self) -> bool {
        false
    }

    fn deep_clone_socket(&self) -> Arc<dyn File> {
        todo!()
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
        let socket_handler = NET_INTERFACE.add_socket(socket);
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
        }
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
        NET_INTERFACE.inner_handler(|inner| {
            let socket = inner.sockets.get_mut::<tcp::Socket>(handler);
            let ret = socket.connect(inner.iface.context(), remote_endpoint, local);
            if ret.is_err() {
                log::info!("[Tcp::connect] {} connect error occur", handler);
                match ret.err().unwrap() {
                    tcp::ConnectError::Unaddressable => return Err(SyscallErr::EINVAL),
                    tcp::ConnectError::InvalidState => return Err(SyscallErr::EISCONN),
                }
            }
            info!("berfore poll socket state: {}", socket.state());
            Ok(())
        })?;
        Ok(())
    }
    fn _accept(&self, nonblock: bool) -> GeneralRet<(SocketHandle, IpEndpoint)> {
        if self.is_shutdown.load(Ordering::Acquire) {
            log::info!("[TcpSocket::_accept] socket is shutdown, cannot accept new connections");
            return Err(SyscallErr::ENOTCONN);
        }

        NET_INTERFACE.poll();
        let mut found_handler = None;
        let mut peer_endpoint = None;

        let handlers: Vec<SocketHandle> = { self.handlers.lock().iter().copied().collect() };
        for handler in handlers {
            let (state, remote) =
                NET_INTERFACE.tcp_socket(handler, |s| (s.state(), s.remote_endpoint()));
            if state == tcp::State::SynReceived || state == tcp::State::Established {
                found_handler = Some(handler);
                peer_endpoint = remote;
                break;
            }
        }

        if let Some(handler_to_remove) = found_handler {
            log::info!(
                "[Socket::accept] found new connection {}, peer {:?}",
                handler_to_remove,
                peer_endpoint
            );
            // 移除监听池中已经连接的socket
            let mut queue = self.handlers.lock();
            if let Some(pos) = queue.iter().position(|&h| h == handler_to_remove) {
                queue.remove(pos);
            }

            //补充新的listener
            let local_ep = self.inner.lock().local_endpoint;
            let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
            let mut new_socket = socket::tcp::Socket::new(rx_buf, tx_buf);
            new_socket.listen(local_ep).unwrap(); //如果unwrap失败说明底层有问题
            let new_handler = NET_INTERFACE.add_socket(new_socket);
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
