use alloc::vec::Vec;

use crate::fs::iov::IOVec;
use crate::mm::{
    copy_from_user_array, translated_byte_buffer_append_to_existing_vec, translated_ref,
    UserAccess, UserBuffer,
};
use crate::net::config::NET_INTERFACE;
use crate::net::posix::MsgHdr;
use crate::net::{Endpoint, PSOCK};
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use super::common::MsgFlags;

pub fn sys_sendmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let msgdontwait = match msg_flags.validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(f) => f.clone(),
        Err(e) => return e,
    };
    let is_nonblock = socket_file.get_nonblock() || msgdontwait;

    let token = task.get_user_token();
    let msg = match translated_ref(token, msg_ptr as *const MsgHdr) {
        Ok(m) => *m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    // 读取 iovec 数组
    let iov_cnt = msg.msg_iovlen;
    if iov_cnt > 1024 {
        return -(SyscallErr::EINVAL as isize);
    }
    let mut iovecs = alloc::vec![IOVec {iov_base: core::ptr::null(), iov_len: 0}; iov_cnt];
    if copy_from_user_array(token, msg.msg_iov, iovecs.as_mut_ptr(), iov_cnt).is_err() {
        return -(SyscallErr::EFAULT as isize);
    }

    // 从用户 iovec 读取数据到内核 flat buffer
    let total_len: usize = iovecs.iter().map(|iov| iov.iov_len).sum();
    if total_len > 64 * 1024 * 1024 {
        return -(SyscallErr::ENOBUFS as isize);
    }
    let mut buf_parts = Vec::new();
    for iov in &iovecs {
        if iov.iov_len == 0 {
            continue;
        }
        match translated_byte_buffer_append_to_existing_vec(
            &mut buf_parts,
            token,
            iov.iov_base,
            iov.iov_len,
            UserAccess::Read,
        ) {
            Ok(_) => {}
            Err(e) => return e,
        }
    }
    let mut buf = alloc::vec![0u8; total_len];
    {
        let user_buf = UserBuffer::new(buf_parts);
        user_buf.read(&mut buf);
    }

    // 解析目标地址（msg_name）为 Endpoint
    let dest_endpoint = if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        let copy_len = (msg.msg_namelen as usize).min(128);
        let mut addr_parts = Vec::new();
        match translated_byte_buffer_append_to_existing_vec(
            &mut addr_parts,
            token,
            msg.msg_name,
            copy_len,
            UserAccess::Read,
        ) {
            Ok(_) => {
                let mut addr_buf = [0u8; 128];
                let addr_user_buf = UserBuffer::new(addr_parts);
                addr_user_buf.read(&mut addr_buf[..copy_len]);
                match Endpoint::from_sockaddr(&addr_buf[..copy_len]) {
                    Ok(ep) => Some(ep),
                    Err(_) => return -(SyscallErr::EINVAL as isize),
                }
            }
            Err(e) => return e,
        }
    } else {
        None
    };

    let socket = crate::get_socket!(sockfd);
    match socket.socket_type() {
        PSOCK::Datagram => {
            // Auto-bind if not bound (same as sys_sendto)
            if socket
                .local_endpoint()
                .map(|ep| ep.port() == 0)
                .unwrap_or(true)
            {
                let auto_bind = Endpoint::Ip(IpEndpoint::new(
                    IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
                    0,
                ));
                let _ = socket.bind(&auto_bind);
            }
            // sendmsg without msg_name on unconnected DGRAM → EDESTADDRREQ
            if dest_endpoint.is_none() && socket.remote_endpoint().is_none() {
                return -(SyscallErr::EDESTADDRREQ as isize);
            }
            wait_io(
                || socket.try_sendmsg(&buf, dest_endpoint.clone(), msg_flags),
                is_nonblock,
            )
        }
        PSOCK::Stream => {
            let wq = socket.send_wait_queue().unwrap();
            if is_nonblock {
                NET_INTERFACE.try_poll();
                match socket.try_sendmsg(&buf, None, msg_flags) {
                    Ok(n) => n as isize,
                    Err(e) => -(e as isize),
                }
            } else {
                WaitQueue::wait_until_interruptible(wq, || {
                    match socket.try_sendmsg(&buf, None, msg_flags) {
                        Ok(n) => Some(n as isize),
                        Err(SyscallErr::EAGAIN) => None,
                        Err(e) => Some(-(e as isize)),
                    }
                })
                .unwrap_or_else(|e| e)
            }
        }
        PSOCK::Raw => wait_io(
            || socket.try_sendmsg(&buf, dest_endpoint.clone(), msg_flags),
            is_nonblock,
        ),
        _ => wait_io(
            || socket.try_sendmsg(&buf, dest_endpoint.clone(), msg_flags),
            is_nonblock,
        ),
    }
}
