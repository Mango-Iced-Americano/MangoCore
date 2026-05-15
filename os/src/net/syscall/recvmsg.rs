use alloc::vec::Vec;

use crate::mm::{UserIoVec, UserPtr, UserPtrMut};
use crate::net::config::NET_INTERFACE;
use crate::net::posix::MsgHdr;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

const MAX_MSG_IO_SIZE: usize = 64 * 1024 * 1024;

pub fn sys_recvmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msgdontwait = match MsgFlags::from_bits_truncate(flags).validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(f) => f.clone(),
        Err(e) => return e,
    };
    let is_nonblock = socket_file.get_nonblock() || msgdontwait;

    // 读取 MsgHdr
    let msg = match UserPtr::<MsgHdr>::from_addr(msg_ptr).read(token) {
        Ok(m) => m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    // 读取 iovec 数组
    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        msg.msg_iov as *const crate::fs::iov::IOVec,
        msg.msg_iovlen,
        MAX_MSG_IO_SIZE,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };

    // 分配接收缓冲区
    if user_iov.total_len() > MAX_MSG_IO_SIZE {
        return -(SyscallErr::ENOBUFS as isize);
    }
    let mut buf = Vec::new();
    if buf.try_reserve(user_iov.total_len()).is_err() {
        return -(SyscallErr::ENOBUFS as isize);
    }
    unsafe {
        buf.set_len(user_iov.total_len());
    }

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
    let mut write_buf = match user_iov.writer_buffer() {
        Ok(buffer) => buffer,
        Err(errno) => return errno,
    };
    write_buf.write(&buf[..nbytes]);

    // 写回源地址（msg_name）
    if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        if let Some(src_addr) = socket.last_recv_addr() {
            let namelen_field_offset = msg_ptr + core::mem::offset_of!(MsgHdr, msg_namelen);
            let _ = src_addr.fill_sockaddr(msg.msg_name as usize, namelen_field_offset);
        }
    }

    // 写回 msg_controllen = 0, msg_flags = 0
    let msg_controllen = 0usize;
    if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_controllen))
        .write(token, &msg_controllen)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }
    let msg_flags = 0i32;
    if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_flags))
        .write(token, &msg_flags)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }

    nbytes as isize
}
