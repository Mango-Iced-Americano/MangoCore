use log::info;

use crate::net::config::NET_INTERFACE;
use crate::net::{Endpoint, PSOCK};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;


pub fn sys_sendto(
    sockfd: u32,
    buf: usize,
    len: usize,
    flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize {
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let msg_dontwait = match msg_flags.validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    let buf = crate::trans_ref!(buf, len);
    let socket = crate::get_socket!(sockfd);
    log::info!("[sys_sendto] get socket sockfd: {}", sockfd);
    let is_nonblock = socket_file.get_nonblock() || msg_dontwait;

    // Validate dest_addr/addrlen for connection-mode sockets
    if dest_addr != 0 {
        match socket.socket_type() {
            PSOCK::Stream => {
                // POSIX: sendto on a SOCK_STREAM ignores dest_addr,
                // but we still validate the pointer for EFAULT.
                let _ = crate::trans_ref!(dest_addr, addrlen);
            }
            PSOCK::Datagram => {
                // Validate addrlen: must be at least sizeof(sockaddr_in) = 16, at most 128
                if addrlen < 16 || addrlen > 128 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            _ => {}
        }
    }

    match socket.socket_type() {
        PSOCK::Datagram => {
            if socket.local_endpoint().map(|ep| ep.port() == 0).unwrap_or(true) {
                // 构造 AF_INET:port=0:addr=0 的 sockaddr_in 用于自动绑定
                let auto_bind = Endpoint::Ip(IpEndpoint::new(
                    IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
                    0,
                ));
                let _ = socket.bind(&auto_bind);
            }
            if dest_addr != 0 {
                let dest_addr_buf = crate::trans_ref!(dest_addr, addrlen);
                if let Ok(ep) = Endpoint::from_sockaddr(dest_addr_buf) {
                    let _ = socket.connect(&ep);
                }
            } else if socket.remote_endpoint().is_none() {
                // send() without destination on unconnected DGRAM → EDESTADDRREQ
                return -(SyscallErr::EDESTADDRREQ as isize);
            }
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let ret = match socket.try_send(buf, msg_flags) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || match socket.try_send(buf, msg_flags) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    })
                    .unwrap_or_else(|e| e);
                    NET_INTERFACE.try_poll();
                    ret
                }
            } else {
                wait_io(|| socket.try_send(buf, msg_flags), is_nonblock)
            }
        }
        PSOCK::Stream => {
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let ret = match socket.try_send(buf, msg_flags) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || match socket.try_send(buf, msg_flags) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    })
                    .unwrap_or_else(|e| e);
                    NET_INTERFACE.try_poll();
                    ret
                }
            } else {
                wait_io(|| socket.try_send(buf, msg_flags), is_nonblock)
            }
        }
        PSOCK::Raw => {
            info!("[sys_sendto] socket is raw");
            let dest_buf = crate::trans_ref!(dest_addr, addrlen);
            let dest_endpoint = match Endpoint::from_sockaddr(dest_buf) {
                Ok(ep) => ep,
                Err(e) => return -(e as isize),
            };
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let ret = match socket.send_to(buf, dest_endpoint) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || {
                        match socket.send_to(buf, dest_endpoint.clone()) {
                            Ok(n) => Some(n as isize),
                            Err(SyscallErr::EAGAIN) => None,
                            Err(e) => Some(-(e as isize)),
                        }
                    })
                    .unwrap_or_else(|e| e);
                    NET_INTERFACE.try_poll();
                    ret
                }
            } else {
                wait_io(
                    || socket.send_to(buf, dest_endpoint.clone()).map(|n| n as isize),
                    is_nonblock,
                )
            }
        }
        _ => todo!(),
    }
}
