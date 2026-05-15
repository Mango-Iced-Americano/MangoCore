use alloc::vec::Vec;

use crate::fs::iov::IOVec;
use crate::mm::{
    copy_from_user_array, translated_byte_buffer_append_to_existing_vec, translated_ref,
    translated_ref_write, UserAccess, UserBuffer,
};
use crate::net::config::NET_INTERFACE;
use crate::net::posix::MsgHdr;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

pub fn sys_recvmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msgdontwait = match MsgFlags::from_bits_truncate(flags).validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let is_nonblock = {
        let fd_table = task.files.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msgdontwait;

    // 读取 MsgHdr
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

    // 分配接收缓冲区
    let total_len: usize = iovecs.iter().map(|iov| iov.iov_len).sum();
    if total_len > 64 * 1024 * 1024 {
        return -(SyscallErr::ENOBUFS as isize);
    }
    let mut buf = alloc::vec![0u8; total_len];

    let socket = crate::get_socket!(sockfd);
    let ret = if let Some(wq) = socket.recv_wait_queue() {
        if is_nonblock {
            // Non-blocking: poll before recv to prevent tight-loop starvation
            NET_INTERFACE.try_poll();
            match socket.try_recvmsg(&mut buf) {
                Ok((n, _)) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wq, || match socket.try_recvmsg(&mut buf) {
                Ok((n, _)) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(|| socket.try_recvmsg(&mut buf).map(|(n, _)| n), is_nonblock)
    };

    if ret < 0 {
        return ret;
    }
    let nbytes = ret as usize;

    // 将接收到的数据分散写入用户 iovec
    let mut write_parts = Vec::new();
    for iov in &iovecs {
        if iov.iov_len == 0 {
            continue;
        }
        match translated_byte_buffer_append_to_existing_vec(
            &mut write_parts,
            token,
            iov.iov_base,
            iov.iov_len,
            UserAccess::Write,
        ) {
            Ok(_) => {}
            Err(e) => return e,
        }
    }
    {
        let mut write_buf = UserBuffer::new(write_parts);
        write_buf.write(&buf[..nbytes]);
    }

    // 写回源地址（msg_name）
    if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        if let Some(src_addr) = socket.last_recv_addr() {
            let namelen_field_offset = msg_ptr + core::mem::offset_of!(MsgHdr, msg_namelen);
            let _ = src_addr.fill_sockaddr(msg.msg_name as usize, namelen_field_offset);
        }
    }

    // 写回 msg_controllen = 0, msg_flags = 0
    let write_back = match translated_ref_write(token, msg_ptr as *mut MsgHdr) {
        Ok(m) => m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    write_back.msg_controllen = 0;
    write_back.msg_flags = 0;

    nbytes as isize
}
