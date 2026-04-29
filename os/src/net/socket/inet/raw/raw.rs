#![allow(unused)]

use crate::net::{Mutex, Socket};
use crate::task::manager::WaitQueue;
use crate::{
    net::{config::NET_INTERFACE, MAX_BUFFER_SIZE, SHUT_WR},
    task::{
        block_current_and_run_next, suspend_current_and_run_next, wait_interruptible,
        wait_interruptible_timeout,
    },
    timer::TimeSpec,
    utils::error::{GeneralRet, SyscallErr, SyscallRet},
};
use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use log::info;
use smoltcp::{
    iface::SocketHandle,
    socket::{self, raw, raw::PacketMetadata},
    wire::{IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion},
};

pub struct RawSocket {
    inner: Mutex<RawSocketInner>,
    socket_handler: SocketHandle,
    pub(crate) recv_waiters: Mutex<WaitQueue>,
}

#[allow(unused)]
struct RawSocketInner {
    local_endpoint: Option<IpListenEndpoint>,
    remote_endpoint: Option<IpEndpoint>,
    ip_version: IpVersion,
    ip_protocol: IpProtocol,
    recvbuf_size: usize,
    sendbuf_size: usize,
}

impl Socket for RawSocket {
    fn bind(&self, addr: IpListenEndpoint) -> SyscallRet {
        log::info!("[Raw::bind] bind to {:?}", addr);
        NET_INTERFACE.poll();
        todo!()
    }

    fn listen(&self) -> SyscallRet {
        todo!()
    }

    fn connect<'a>(&'a self, _addr_buf: &'a [u8]) -> SyscallRet {
        todo!()
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        todo!()
    }

    fn socket_type(&self) -> crate::net::SocketType {
        crate::net::SocketType::SOCK_RAW
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
        todo!()
    }

    fn remote_endpoint(&self) -> Option<IpEndpoint> {
        self.inner.lock().remote_endpoint
    }

    fn shutdown(&self, how: u32) -> GeneralRet<()> {
        info!("[RawSocket::shutdown] how {}", how);
        todo!()
    }

    fn set_nagle_enabled(&self, _enabled: bool) -> SyscallRet {
        todo!()
    }

    fn set_keep_alive(&self, _enabled: bool) -> SyscallRet {
        todo!()
    }

    fn reuse_addr(&self) -> SyscallRet {
        todo!()
    }

    fn set_reuse_addr(&self, enabled: bool) -> SyscallRet {
        todo!()
    }

    fn send_to(&self, user_buf: &[u8], dest_addr: IpEndpoint) -> SyscallRet {
        let (version, protocol) = {
            let inner = self.inner.lock();
            (inner.ip_version, inner.ip_protocol)
        };
        match version {
            IpVersion::Ipv4 => {
                let target_ip = match dest_addr.addr {
                    smoltcp::wire::IpAddress::Ipv4(ip) => ip,
                    _ => return Err(SyscallErr::EINVAL),
                };
                let mut packet_buf = vec![0u8; 20 + user_buf.len()];

                log::info!("[RawSocketsendto] make ipv4 head...");
                //封装IP头
                let mut ip_pkg = smoltcp::wire::Ipv4Packet::new_unchecked(&mut packet_buf);
                ip_pkg.set_version(4);
                ip_pkg.set_header_len(20);
                ip_pkg.set_total_len((20 + user_buf.len()) as u16);
                ip_pkg.set_next_header(protocol); // 使用刚才解锁拿到的 protocol
                ip_pkg.set_hop_limit(64);
                ip_pkg.set_dst_addr(target_ip);
                let src_addr = if target_ip.is_loopback() {
                    smoltcp::wire::Ipv4Address([127, 0, 0, 1])
                } else {
                    smoltcp::wire::Ipv4Address([10, 0, 2, 15]) // 或者是你网卡的真实 IP
                };
                ip_pkg.set_src_addr(src_addr); //先硬编码

                ip_pkg.payload_mut().copy_from_slice(user_buf);
                ip_pkg.fill_checksum();

                NET_INTERFACE.poll();
                let ret = NET_INTERFACE
                    .raw_socket(self.socket_handler, |socket| {
                        log::info!(
                            "[RawSocket] Sending {} bytes to {}",
                            user_buf.len(),
                            target_ip
                        );
                        match socket.send_slice(ip_pkg.into_inner()) {
                            Ok(_) => Ok(user_buf.len()),
                            Err(_) => Err(SyscallErr::ENOBUFS),
                        }
                    })
                    .ok_or(SyscallErr::EAGAIN)?;
                NET_INTERFACE.poll();
                ret
            }
            IpVersion::Ipv6 => {
                todo!()
            }
        }
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        // 不调用 poll，只做一次尝试
        NET_INTERFACE
            .raw_socket(self.socket_handler, |socket| {
                if !socket.can_recv() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.recv_slice(buf) {
                    Ok(nbytes) => {
                        let packet = smoltcp::wire::Ipv4Packet::new_unchecked(&buf[..nbytes]);
                        let src_addr = packet.src_addr();
                        self.inner.lock().remote_endpoint =
                            Some(IpEndpoint::new(src_addr.into(), 0));
                        Ok(nbytes as isize)
                    }
                    Err(_) => Err(SyscallErr::ENOTCONN),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        // 不调用 poll，只做一次尝试
        NET_INTERFACE
            .raw_socket(self.socket_handler, |socket| {
                if !socket.can_send() {
                    return Err(SyscallErr::EAGAIN);
                }
                match socket.send_slice(buf) {
                    Ok(()) => Ok(buf.len() as isize),
                    Err(_) => Err(SyscallErr::ENOBUFS),
                }
            })
            .unwrap_or(Err(SyscallErr::EAGAIN))
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.recv_waiters)
    }
}

impl RawSocket {
    pub fn new(protocol: u32) -> Self {
        let tx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let rx_buf = socket::raw::PacketBuffer::new(
            vec![PacketMetadata::EMPTY; 128],
            vec![0 as u8; MAX_BUFFER_SIZE],
        );
        let socket = raw::Socket::new(
            smoltcp::wire::IpVersion::Ipv4,
            smoltcp::wire::IpProtocol::from(protocol as u8),
            rx_buf,
            tx_buf,
        );
        let socket_handler = NET_INTERFACE.add_socket(socket).unwrap();
        log::info!("[RawSocket::new] new {}", socket_handler);
        NET_INTERFACE.poll();
        let inner = RawSocketInner {
            local_endpoint: None,
            remote_endpoint: None,
            ip_version: IpVersion::Ipv4,
            ip_protocol: IpProtocol::from(protocol as u8),
            recvbuf_size: MAX_BUFFER_SIZE,
            sendbuf_size: MAX_BUFFER_SIZE,
        };

        Self {
            inner: Mutex::new(inner),
            recv_waiters: Mutex::new(WaitQueue::new()),
            socket_handler,
        }
    }
}

impl RawSocket {
    /// 注册 raw socket 到全局表，供 wake_raw_waiters 使用
    pub fn register_raw_socket(socket: &Arc<Self>) {
        crate::net::RAW_SOCKETS
            .lock()
            .push((socket.socket_handler, Arc::downgrade(socket)));
    }
}
