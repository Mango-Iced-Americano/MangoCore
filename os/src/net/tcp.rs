use super::{Mutex, Socket};
use crate::{
    fs::{file_trait::File, FileDescriptor, OpenFlags},
    net::{
        address,
        config::{lookup_source_ip, NET_INTERFACE},
        MAX_BUFFER_SIZE, SHUT_WR,
    },
    task::{current_task, wait_interruptible},
    utils::{
        error::{GeneralRet, SyscallErr, SyscallRet},
        random::RNG,
    },
};
use alloc::{sync::Arc, vec};
use core::time::Duration;
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
use alloc::string::String;
use alloc::sync::Weak;
use alloc::vec::Vec;

pub const TCP_MSS_DEFAULT: u32 = 1 << 15;
pub const TCP_MSS: u32 = if TCP_MSS_DEFAULT > MAX_BUFFER_SIZE as u32 {
    MAX_BUFFER_SIZE as u32
} else {
    TCP_MSS_DEFAULT
};

pub struct TcpSocket {
    inner: Mutex<TcpSocketInner>,
}

#[allow(unused)]
struct TcpSocketInner {
    socket_handler: SocketHandle,
    local_endpoint: IpListenEndpoint,
    remote_endpoint: Option<IpEndpoint>,
    last_state: tcp::State,
    recvbuf_size: usize,
    sendbuf_size: usize,
    is_listing: bool,
    // TODO: add more
}

impl Socket for TcpSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        info!("[tcp::bind] bind to: {:?}", addr);
        self.inner.lock().local_endpoint = addr;
        Ok(0)
    }

    fn listen(&self) -> SyscallRet {
        let (local, handler) = {
            let inner = self.inner.lock();
            (inner.local_endpoint, inner.socket_handler)
        };
        info!("[Tcp::listen] {} listening: {:?}", handler, local);
        NET_INTERFACE.tcp_socket(handler, |socket| {
            socket.listen(local).ok().ok_or(SyscallErr::EADDRINUSE)
        })?;
        // update last_state outside of NET_INTERFACE closure to avoid locking inner inside
        let state = NET_INTERFACE.tcp_socket(handler, |socket| socket.state());
        self.inner.lock().last_state = state;
        self.inner.lock().is_listing = true;
        Ok(0)
    }

    fn accept(&self, sockfd: u32, addr: usize, addrlen: usize) -> crate::utils::error::SyscallRet {
        // get old socket
        let task = current_task().unwrap();
        let old_nonblock = task
            .files
            .lock()
            .get_ref(sockfd as usize)
            .unwrap()
            .get_nonblock();
        let peer_addr = self._accept(old_nonblock)?;
        log::info!("[Socket::accept] connection established");

        let mut inner = self.inner.lock();
        let connected_handler = inner.socket_handler;

        let tx_buf = socket::tcp::SocketBuffer::new(vec![0u8; MAX_BUFFER_SIZE]);
        let rx_buf = socket::tcp::SocketBuffer::new(vec![0u8; MAX_BUFFER_SIZE]);
        let mut new_socket = socket::tcp::Socket::new(rx_buf, tx_buf);

        new_socket.listen(inner.local_endpoint).unwrap();
        let new_listener_handler = NET_INTERFACE.add_socket(new_socket);

        inner.socket_handler = new_listener_handler; //将原本位置的socket handler重新换成listen状态的handler

        let connected_socket = Arc::new(TcpSocket {
            inner: Mutex::new(TcpSocketInner {
                socket_handler: connected_handler,
                local_endpoint: inner.local_endpoint,
                remote_endpoint: Some(peer_addr),
                last_state: tcp::State::Established,
                recvbuf_size: inner.recvbuf_size,
                sendbuf_size: inner.sendbuf_size,
                is_listing: false,
            }),
        });
        drop(inner);

        let mut fd_table = task.files.lock();
        let mut socket_table = task.socket_table.lock();

        let old_cloexec = fd_table.get_ref(sockfd as usize).unwrap().get_cloexec();

        let new_fd = fd_table
            .insert(FileDescriptor::new(
                old_cloexec,
                old_nonblock,
                connected_socket.clone(),
            ))
            .unwrap();

        socket_table.insert(new_fd, connected_socket);

        address::fill_with_endpoint(peer_addr, addr, addrlen)?;

        Ok(new_fd)
    }

    fn socket_type(&self) -> super::SocketType {
        super::SocketType::SOCK_STREAM
    }

    fn connect<'a>(&'a self, addr_buf: &'a [u8]) -> crate::utils::error::SyscallRet {
        let remote_endpoint = address::endpoint(addr_buf)?;
        self._connect(remote_endpoint)?;
        loop {
            let handler = self.inner.lock().socket_handler;
            let state = NET_INTERFACE.tcp_socket(handler, |socket| socket.state());
            match state {
                tcp::State::Closed => {
                    // close but not already connect, retry
                    info!(
                        "[Tcp::connect] {} already closed, try again",
                        self.inner.lock().socket_handler
                    );
                    self._connect(remote_endpoint)?;
                }
                tcp::State::Established => {
                    info!(
                        "[Tcp::connect] {} connected, state {:?}",
                        self.inner.lock().socket_handler,
                        state
                    );
                    return Ok(0);
                }
                _ => {
                    log::trace!(
                        "[Tcp::connect] {} not connect yet, state {:?}",
                        self.inner.lock().socket_handler,
                        state
                    );
                }
            }
            suspend_current_and_run_next();
            // thread::sleep(Duration::from_secs(1));
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
        NET_INTERFACE.poll();
        let handler = self.inner.lock().socket_handler;
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| socket.remote_endpoint());
        NET_INTERFACE.poll();
        ret
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        info!("[TcpSocket::shutdown] how {}", how);
        let handler = self.inner.lock().socket_handler;
        NET_INTERFACE.tcp_socket(handler, |socket| match how {
            SHUT_WR => socket.close(),
            _ => socket.abort(),
        });
        NET_INTERFACE.poll();
        Ok(())
    }

    fn set_nagle_enabled(&self, enabled: bool) -> SyscallRet {
        let handler = self.inner.lock().socket_handler;
        NET_INTERFACE.tcp_socket(handler, |socket| socket.set_nagle_enabled(enabled));
        Ok(0)
    }

    fn set_keep_alive(&self, enabled: bool) -> SyscallRet {
        if enabled {
            let handler = self.inner.lock().socket_handler;
            NET_INTERFACE.tcp_socket(handler, |socket| {
                socket.set_keep_alive(Some(Duration::from_secs(1).into()))
            });
        }
        Ok(0)
    }

    fn send_to(&self, buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet {
        todo!();
    }
}

impl TcpSocket {
    pub fn new() -> Self {
        let tx_buf = socket::tcp::SocketBuffer::new(vec![0 as u8; MAX_BUFFER_SIZE]);
        let rx_buf = socket::tcp::SocketBuffer::new(vec![0 as u8; MAX_BUFFER_SIZE]);
        let socket = socket::tcp::Socket::new(rx_buf, tx_buf);
        let socket_handler = NET_INTERFACE.add_socket(socket);
        log::info!("[TcpSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        Self {
            inner: Mutex::new(TcpSocketInner {
                socket_handler,
                local_endpoint: IpListenEndpoint {
                    addr: None,
                    port: unsafe { RNG.positive_u32() as u16 },
                },
                remote_endpoint: None,
                last_state: tcp::State::Closed,
                recvbuf_size: MAX_BUFFER_SIZE,
                sendbuf_size: MAX_BUFFER_SIZE,
                is_listing: false,
            }),
        }
    }

    fn _connect(&self, remote_endpoint: IpEndpoint) -> GeneralRet<()> {
        self.inner.lock().remote_endpoint = Some(remote_endpoint);
        let mut local = self.inner.lock().local_endpoint;
        if local.addr.is_none() {
            local.addr = Some(lookup_source_ip(remote_endpoint.addr));
        }
        info!(
            "[Tcp::connect] local: {:?}, remote: {:?}",
            local, remote_endpoint
        );
        let handler = self.inner.lock().socket_handler;
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
    fn _accept(&self, nonblock: bool) -> GeneralRet<IpEndpoint> {
        loop {
            NET_INTERFACE.poll();
            let handler = self.inner.lock().socket_handler;
            let ret = NET_INTERFACE.tcp_socket(handler, |socket| {
                if !socket.is_open() {
                    log::info!("[TcpAcceptFuture::poll] this socket is not open");
                    return Err(SyscallErr::EINVAL);
                }
                if socket.state() == tcp::State::SynReceived
                    || socket.state() == tcp::State::Established
                {
                    log::info!("[TcpAcceptFuture::poll] state become {:?}", socket.state());
                    return Ok(socket.remote_endpoint().unwrap());
                }
                // log::info!(
                //     "[TcpAcceptFuture::poll] not syn yet, state {:?}",
                //     socket.state()
                // );
                if nonblock {
                    log::info!("[TcpAcceptFuture::poll] flags set nonblock");
                    return Err(SyscallErr::EAGAIN);
                }
                // 使用 continue 跳过当前循环并开始下一次迭代
                return Err(SyscallErr::EAGAIN);
            });
            NET_INTERFACE.poll();
            match ret {
                Ok(endpoint) => {
                    // update last_state outside closure to avoid double-locking
                    let state = NET_INTERFACE.tcp_socket(handler, |socket| socket.state());
                    self.inner.lock().last_state = state;
                    self.inner.lock().is_listing = false;
                    return GeneralRet::Ok(endpoint);
                }
                Err(SyscallErr::EAGAIN) => {
                    if nonblock {
                        return GeneralRet::Err(SyscallErr::EAGAIN);
                    }
                    suspend_current_and_run_next();
                    // 如果返回 EAGAIN 错误，继续循环
                    // wait_interruptible();
                    continue;
                }
                Err(err) => return GeneralRet::Err(err),
            }
        }
    }
}
use crate::task::suspend_current_and_run_next;
impl Drop for TcpSocket {
    fn drop(&mut self) {
        let (handler, localep) = {
            let inner = self.inner.lock();
            (inner.socket_handler, inner.local_endpoint)
        };
        info!(
            "[TcpSocket::drop] drop socket {}, localep {:?}",
            handler, localep
        );
        NET_INTERFACE.tcp_socket(handler, |socket| {
            info!("[TcpSocket::drop] before state is {:?}", socket.state());
            if socket.is_open() {
                socket.close();
            }
            info!("[TcpSocket::drop] after state is {:?}", socket.state());
        });
        NET_INTERFACE.poll();
        NET_INTERFACE.remove(handler);
        NET_INTERFACE.poll();
    }
}

impl File for TcpSocket {
    fn deep_clone(&self) -> Arc<dyn File> {
        todo!();
    }
    fn readable(&self) -> bool {
        true
    }
    fn writable(&self) -> bool {
        true
    }
    fn read(&self, _offset: Option<&mut usize>, buf: &mut [u8]) -> usize {
        match self._read(buf) {
            Ok(ret) => ret,
            Err(err) => err.as_errno_ret(),
        }
    }
    fn write(&self, _offset: Option<&mut usize>, buf: &[u8]) -> usize {
        NET_INTERFACE.poll();
        let handler = self.inner.lock().socket_handler;
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| {
            if !socket.may_send() {
                log::info!("[TcpSendFuture::poll] err when send");
                return (SyscallErr::ENOTCONN).as_errno_ret();
            }
            if !socket.can_send() {
                log::trace!("[TcpSendFuture::poll] cannot send yet");
                // suspend_current_and_run_next();
                return (SyscallErr::EAGAIN).as_errno_ret();
            }
            log::info!("[TcpSendFuture::poll] start to send...");
            info!(
                "[TcpSendFuture::poll] {:?} -> {:?}",
                socket.local_endpoint(),
                socket.remote_endpoint()
            );
            match socket.send_slice(buf) {
                Ok(nbytes) => {
                    info!("[TcpSendFuture::poll] send {} bytes", nbytes);
                    return nbytes;
                }
                Err(_) => (SyscallErr::ENOTCONN).as_errno_ret(),
            }
        });
        NET_INTERFACE.poll();
        ret
    }
    fn r_ready(&self) -> bool {
        let (is_listener, handler) = {
            let inner = self.inner.lock();
            (inner.is_listing, inner.socket_handler)
        };
        NET_INTERFACE.poll();
        let mut ret = false;
        NET_INTERFACE.tcp_socket(handler, |socket| {
            if is_listener {
                let state = socket.state();
                ret = state == tcp::State::SynReceived || state == tcp::State::Established
            } else {
                let can_recv = socket.can_recv();
                let may_recv = socket.may_recv();
                let state = socket.state();
                let is_connecting =
                    state == tcp::State::SynSent || state == tcp::State::SynReceived;
                // 如果不能 recv 且不是正在连接，说明是对端断开/连接错误 (EOF)
                let is_eof_or_error = !may_recv && !is_connecting && state != tcp::State::Listen;
                if !may_recv || can_recv {
                    log::info!(
                        "DEBUG: Socket {} r_ready! state={:?}, can_recv={}, may_recv={}",
                        handler,
                        state,
                        can_recv,
                        may_recv
                    );
                }
                ret = can_recv || is_eof_or_error;
            }
        });
        log::info!("[TcpSocket::r_ready] socket {}, r_ready: {}", handler, ret);
        ret
    }
    fn w_ready(&self) -> bool {
        let handler = self.inner.lock().socket_handler;
        NET_INTERFACE.poll();
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| {
            let state = socket.state();
            if state == tcp::State::Closed {
                true
            } else {
                socket.can_send() && socket.may_send()
            }
        });
        log::info!("[TcpSocket::w_ready] socket {}, w_ready: {}", handler, ret);
        ret
    }
    fn read_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        // let mut buffers = buf.buffers;
        // let buf = unsafe {
        //     core::slice::from_raw_parts_mut(buffers[0].as_mut_ptr() as *mut u8, buf.len as usize)
        // };
        // let ret = self._read(buf);
        // match ret {
        //     Ok(s) => return s,
        //     Err(err) => return err.as_errno_ret(),
        // }
        let mut data = vec![0u8; buf.len];
        let ret = self._read(&mut data);
        match ret {
            Ok(s) => {
                let mut offset = 0;
                let mut remain = s;
                // 安全地将数据分布写回到分散的物理页切片中
                for b in buf.buffers.into_iter() {
                    let copy_len = remain.min(b.len());
                    b[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
                    offset += copy_len;
                    remain -= copy_len;
                    if remain == 0 {
                        break;
                    }
                }
                s
            }
            Err(err) => err.as_errno_ret(),
        }
    }
    fn write_user(&self, _offset: Option<usize>, buf: UserBuffer) -> usize {
        // let mut buffers = buf.buffers;
        // let buf = unsafe {
        //     core::slice::from_raw_parts_mut(buffers[0].as_mut_ptr() as *mut u8, buf.len as usize)
        // };
        let mut data = vec![0u8; buf.len];
        let mut offset = 0;
        // 安全地从分散的物理页切片中收集数据
        for b in buf.buffers.into_iter() {
            data[offset..offset + b.len()].copy_from_slice(&b);
            offset += b.len();
        }
        self.write(None, &data)
    }
    fn get_size(&self) -> usize {
        todo!();
    }
    fn get_stat(&self) -> Stat {
        todo!();
    }
    fn get_file_type(&self) -> DiskInodeType {
        todo!();
    }
    fn is_dir(&self) -> bool {
        todo!();
    }
    fn is_file(&self) -> bool {
        todo!();
    }
    fn info_dirtree_node(&self, _dirnode_ptr: Weak<DirectoryTreeNode>) {
        todo!();
    }
    fn get_dirtree_node(&self) -> Option<Arc<DirectoryTreeNode>> {
        todo!();
    }
    /// open
    fn open(&self, _flags: OpenFlags, _special_use: bool) -> Arc<dyn File> {
        todo!();
    }
    fn open_subfile(&self) -> Result<Vec<(String, Arc<dyn File>)>, isize> {
        todo!();
    }
    /// create
    fn create(&self, _name: &str, _file_type: DiskInodeType) -> Result<Arc<dyn File>, isize> {
        todo!();
    }
    fn link_child(&self, _name: &str, _child: &Self) -> Result<(), isize> {
        todo!();
    }
    /// delete(unlink)
    fn unlink(&self, _delete: bool) -> Result<(), isize> {
        todo!();
    }
    /// dirent
    fn get_dirent(&self, _count: usize) -> Vec<Dirent> {
        todo!();
    }
    /// offset
    fn get_offset(&self) -> usize {
        todo!();
    }
    fn lseek(&self, _offset: isize, _whence: SeekWhence) -> Result<usize, isize> {
        todo!();
    }
    /// size
    fn modify_size(&self, _diff: isize) -> Result<(), isize> {
        todo!();
    }
    fn truncate_size(&self, _new_size: usize) -> Result<(), isize> {
        todo!();
    }
    // time
    fn set_timestamp(&self, _ctime: Option<usize>, _atime: Option<usize>, _mtime: Option<usize>) {
        todo!();
    }
    /// cache
    fn get_single_cache(&self, _offset: usize) -> Result<Arc<Mutex<PageCache>>, ()> {
        todo!();
    }
    fn get_all_caches(&self) -> Result<Vec<Arc<Mutex<PageCache>>>, ()> {
        todo!();
    }
    /// memory related
    fn oom(&self) -> usize {
        todo!();
    }
    /// poll, select related
    fn hang_up(&self) -> bool {
        todo!();
    }
    /// iotcl
    fn ioctl(&self, _cmd: u32, _argp: usize) -> isize {
        todo!();
    }
    /// fcntl
    fn fcntl(&self, _cmd: u32, _arg: u32) -> isize {
        todo!();
    }
}

impl TcpSocket {
    fn _read<'a>(&'a self, buf: &'a mut [u8]) -> GeneralRet<usize> {
        NET_INTERFACE.poll();
        let handler = self.inner.lock().socket_handler;
        let ret = NET_INTERFACE.tcp_socket(handler, |socket| {
            if socket.state() == tcp::State::CloseWait
                || socket.state() == tcp::State::TimeWait
                || socket.state() == tcp::State::FinWait2
            {
                log::info!("[TcpRecvFuture::poll] state become {:?}", socket.state());
                return Ok(0);
            }
            if !socket.may_recv() {
                log::info!(
                    "[TcpRecvFuture::poll] err when recv, state {:?}",
                    socket.state()
                );
                return Err(SyscallErr::ENOTCONN);
            }
            log::trace!("[TcpRecvFuture::poll] state {:?}", socket.state());
            if !socket.can_recv() {
                // panic!();
                log::trace!("[TcpRecvFuture::poll] cannot recv yet");
                log::debug!(
                    "[TcpDebug] RecvQueue: {} bytes, State: {:?}, MayRecv: {}",
                    socket.recv_queue(), // 看看这里到底是不是 0
                    socket.state(),
                    socket.may_recv()
                );
                return Err(SyscallErr::EAGAIN);
            }
            log::info!("[TcpRecvFuture::poll] start to recv...");
            info!(
                "[TcpRecvFuture::poll] {:?} <- {:?}",
                socket.local_endpoint(),
                socket.remote_endpoint()
            );
            match socket.recv_slice(buf) {
                Ok(nbytes) => {
                    info!("[TcpRecvFuture::poll] recv {} bytes", nbytes);
                    Ok(nbytes)
                }
                Err(_) => return Err(SyscallErr::ENOTCONN),
            }
        });
        NET_INTERFACE.poll();
        match ret {
            Ok(result) => return GeneralRet::Ok(result),
            Err(err) => return GeneralRet::Err(err),
        }
    }
}
