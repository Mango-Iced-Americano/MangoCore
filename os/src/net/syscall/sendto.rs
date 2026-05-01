use log::info;

use crate::net::address::{self, SocketAddrv4};
use crate::net::config::NET_INTERFACE;
use crate::net::SocketType;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;
use smoltcp::wire::IpListenEndpoint;

pub fn sys_sendto(
    sockfd: u32,
    buf: usize,
    len: usize,
    flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize {
    let msg_dontwait = match MsgFlags::from_bits_truncate(flags).validate_for_send() {
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
            SocketType::SOCK_STREAM => {
                // Linux: sendto on a SOCK_STREAM with non-NULL dest_addr returns EISCONN
                return -(SyscallErr::EISCONN as isize);
            }
            SocketType::SOCK_DGRAM => {
                // Validate addrlen: must be at least sizeof(sockaddr_in) = 16, at most 128
                if addrlen < 16 || addrlen > 128 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            _ => {}
        }
    }

    match socket.socket_type() {
        SocketType::SOCK_DGRAM => {
            if socket.local_endpoint().port == 0 {
                let addr = SocketAddrv4::new([0; 16].as_slice());
                let endpoint = IpListenEndpoint::from(addr);
                let _ = socket.bind(endpoint);
            }
            let dest_addr = crate::trans_ref!(dest_addr, addrlen);
            let _ = socket.connect(dest_addr);
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    match socket.try_send(buf) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    }
                } else {
                    WaitQueue::wait_until_interruptible(wait_queue, || match socket.try_send(buf) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    })
                    .unwrap_or_else(|e| e)
                }
            } else {
                wait_io(|| socket.try_send(buf), is_nonblock)
            }
        }
        SocketType::SOCK_STREAM => {
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    match socket.try_send(buf) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    }
                } else {
                    WaitQueue::wait_until_interruptible(wait_queue, || match socket.try_send(buf) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    })
                    .unwrap_or_else(|e| e)
                }
            } else {
                wait_io(|| socket.try_send(buf), is_nonblock)
            }
        }
        SocketType::SOCK_RAW => {
            info!("[sys_sendto] socket is raw");
            let dest_addr = crate::trans_ref!(dest_addr, addrlen);
            let endpoint = address::endpoint(dest_addr).unwrap();
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    match socket.send_to(buf, endpoint) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    }
                } else {
                    WaitQueue::wait_until_interruptible(wait_queue, || {
                        match socket.send_to(buf, endpoint) {
                            Ok(n) => Some(n as isize),
                            Err(SyscallErr::EAGAIN) => None,
                            Err(e) => Some(-(e as isize)),
                        }
                    })
                    .unwrap_or_else(|e| e)
                }
            } else {
                wait_io(
                    || socket.send_to(buf, endpoint).map(|n| n as isize),
                    is_nonblock,
                )
            }
        }
        _ => todo!(),
    }
}
