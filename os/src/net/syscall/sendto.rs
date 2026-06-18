use log::info;

use crate::mm::{copy_from_user_array, UserBufferReader};
use crate::net::config::NET_INTERFACE;
use crate::net::{Endpoint, PSOCK};
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;
use smoltcp::wire::{IpAddress, IpEndpoint};

use super::common::MsgFlags;

pub fn sys_sendto(
    sockfd: u32,
    buf: usize,
    len: usize,
    flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize {
    let len = len.min(64 * 1024 * 1024);
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let msg_dontwait = match msg_flags.validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let socket = crate::get_socket!(sockfd);
    log::info!("[sys_sendto] get socket sockfd: {}", sockfd);
    let is_nonblock = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msg_dontwait;

    // Validate dest_addr/addrlen for connection-mode sockets
    if dest_addr != 0 {
        match socket.socket_type() {
            PSOCK::Stream => {
                // POSIX: sendto on a SOCK_STREAM ignores dest_addr,
                // but we still validate the pointer for EFAULT.
                let _ = crate::trans_ref!(dest_addr, addrlen);
            }
            PSOCK::Datagram => {
                // AF_UNIX sockaddr_un may be shorter than sockaddr_in; parse by family later.
                if addrlen < 2 || addrlen > 128 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            _ => {}
        }
    }

    match socket.socket_type() {
        PSOCK::Datagram => {
            if socket
                .local_endpoint()
                .map(|ep| ep.port() == 0)
                .unwrap_or(true)
            {
                // 构造 AF_INET:port=0:addr=0 的 sockaddr_in 用于自动绑定
                let auto_bind = Endpoint::Ip(IpEndpoint::new(
                    IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
                    0,
                ));
                let _ = socket.bind(&auto_bind);
            }
            let dest_endpoint = if dest_addr != 0 {
                let dest_buf = crate::trans_ref!(dest_addr, addrlen);
                match Endpoint::from_sockaddr(dest_buf) {
                    Ok(ep) => Some(ep),
                    Err(e) => return -(e as isize),
                }
            } else {
                None
            };

            if dest_endpoint.is_none() && socket.remote_endpoint().is_none() {
                // 无目的地址且未 connect → EDESTADDRREQ
                return -(SyscallErr::EDESTADDRREQ as isize);
            }
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let reader = match UserBufferReader::new(token, buf as *const u8, len) {
                        Ok(r) => r,
                        Err(e) => return e,
                    };
                    let ubuf = reader.into_user_buffer();
                    let ret = match socket.try_sendmsg_user(&ubuf, dest_endpoint, msg_flags) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let mut kernel_buf = alloc::vec![0u8; len];
                    if copy_from_user_array(
                        token,
                        buf as *const u8,
                        kernel_buf.as_mut_ptr(),
                        len,
                    )
                    .is_err()
                    {
                        return -(SyscallErr::EFAULT as isize);
                    }
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || {
                        match socket.try_sendmsg(&kernel_buf, dest_endpoint.clone(), msg_flags) {
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
                let mut kernel_buf = alloc::vec![0u8; len];
                if copy_from_user_array(
                    token,
                    buf as *const u8,
                    kernel_buf.as_mut_ptr(),
                    len,
                )
                .is_err()
                {
                    return -(SyscallErr::EFAULT as isize);
                }
                wait_io(
                    || socket.try_sendmsg(&kernel_buf, dest_endpoint.clone(), msg_flags),
                    is_nonblock,
                )
            }
        }
        PSOCK::Stream => {
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let reader = match UserBufferReader::new(token, buf as *const u8, len) {
                        Ok(r) => r,
                        Err(e) => return e,
                    };
                    let ubuf = reader.into_user_buffer();
                    let ret = match socket.try_send_user(&ubuf, msg_flags) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let mut kernel_buf = alloc::vec![0u8; len];
                    if copy_from_user_array(
                        token,
                        buf as *const u8,
                        kernel_buf.as_mut_ptr(),
                        len,
                    )
                    .is_err()
                    {
                        return -(SyscallErr::EFAULT as isize);
                    }
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || {
                        match socket.try_send(&kernel_buf, msg_flags) {
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
                let mut kernel_buf = alloc::vec![0u8; len];
                if copy_from_user_array(
                    token,
                    buf as *const u8,
                    kernel_buf.as_mut_ptr(),
                    len,
                )
                .is_err()
                {
                    return -(SyscallErr::EFAULT as isize);
                }
                wait_io(|| socket.try_send(&kernel_buf, msg_flags), is_nonblock)
            }
        }
        PSOCK::Raw => {
            info!("[sys_sendto] socket is raw");
            let dest_endpoint = if dest_addr != 0 {
                let dest_buf = crate::trans_ref!(dest_addr, addrlen);
                match Endpoint::from_sockaddr(dest_buf) {
                    Ok(ep) => Some(ep),
                    Err(e) => return -(e as isize),
                }
            } else {
                None
            };
            let mut kernel_buf = alloc::vec![0u8; len];
            if copy_from_user_array(
                token,
                buf as *const u8,
                kernel_buf.as_mut_ptr(),
                len,
            )
            .is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
            if let Some(wait_queue) = socket.send_wait_queue() {
                if is_nonblock {
                    NET_INTERFACE.try_poll();
                    let ret = match socket.try_sendmsg(&kernel_buf, dest_endpoint, msg_flags) {
                        Ok(n) => n as isize,
                        Err(e) => -(e as isize),
                    };
                    NET_INTERFACE.try_poll();
                    ret
                } else {
                    let ret = WaitQueue::wait_until_interruptible(wait_queue, || {
                        match socket.try_sendmsg(&kernel_buf, dest_endpoint.clone(), msg_flags) {
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
                    || socket.try_sendmsg(&kernel_buf, dest_endpoint.clone(), msg_flags),
                    is_nonblock,
                )
            }
        }
        _ => todo!(),
    }
}
