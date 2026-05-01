use alloc::vec::Vec;

use crate::fs::iov::IOVec;
use crate::mm::{
    copy_from_user_array, translated_byte_buffer_append_to_existing_vec, translated_ref,
    translated_refmut, UserBuffer,
};
use crate::net::address;
use crate::net::posix::MsgHdr;
use crate::net::SocketType;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

pub fn sys_sendmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msgdontwait = match MsgFlags::from_bits_truncate(flags).validate_for_send() {
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
    let mut iovecs = alloc::vec![IOVec {iov_base: core::ptr::null(), iov_len: 0}; iov_cnt];
    if copy_from_user_array(token, msg.msg_iov, iovecs.as_mut_ptr(), iov_cnt).is_err() {
        return -(SyscallErr::EFAULT as isize);
    }

    // 从用户 iovec 读取数据到内核 flat buffer
    let total_len: usize = iovecs.iter().map(|iov| iov.iov_len).sum();
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

    // 解析目标地址（msg_name）
    let dest_addr = if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        let copy_len = (msg.msg_namelen as usize).min(128);
        let mut addr_parts = Vec::new();
        match translated_byte_buffer_append_to_existing_vec(
            &mut addr_parts,
            token,
            msg.msg_name,
            copy_len,
        ) {
            Ok(_) => {
                let mut addr_buf = [0u8; 128];
                let addr_user_buf = UserBuffer::new(addr_parts);
                addr_user_buf.read(&mut addr_buf[..copy_len]);
                match address::endpoint(&addr_buf[..copy_len]) {
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
        SocketType::SOCK_DGRAM => wait_io(|| socket.try_sendmsg(&buf, dest_addr), is_nonblock),
        SocketType::SOCK_STREAM => {
            let wq = socket.send_wait_queue().unwrap();
            if is_nonblock {
                match socket.try_sendmsg(&buf, None) {
                    Ok(n) => n as isize,
                    Err(e) => -(e as isize),
                }
            } else {
                WaitQueue::wait_until_interruptible(wq, || match socket.try_sendmsg(&buf, None) {
                    Ok(n) => Some(n as isize),
                    Err(SyscallErr::EAGAIN) => None,
                    Err(e) => Some(-(e as isize)),
                })
                .unwrap_or_else(|e| e)
            }
        }
        SocketType::SOCK_RAW => wait_io(|| socket.try_sendmsg(&buf, dest_addr), is_nonblock),
        _ => wait_io(|| socket.try_sendmsg(&buf, dest_addr), is_nonblock),
    }
}
