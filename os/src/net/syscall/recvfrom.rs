use log::info;

use crate::mm::{translated_ref, translated_refmut};
use crate::net::SocketType;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

pub fn sys_recvfrom(
    sockfd: u32,
    buf: usize,
    len: u32,
    flags: u32,
    src_addr: usize,
    addrlen: usize,
) -> isize {
    let msg_dontwait = match MsgFlags::from_bits_truncate(flags).validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    let socket = crate::get_socket!(sockfd);

    // 在 syscall 入口校验 src_addr 对应的 *addrlen 值
    if src_addr != 0 {
        let token = task.get_user_token();
        match crate::mm::translated_ref(token, addrlen as *const u32) {
            Ok(addrlen_val) => {
                if *addrlen_val < 16 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            Err(_) => return -(SyscallErr::EFAULT as isize),
        }
    }

    let is_nonblock = {
        let fd_table = task.files.lock();
        fd_table
            .get_ref(sockfd as usize)
            .map(|fd| fd.get_nonblock())
            .unwrap_or(false)
    } || msg_dontwait;

    info!("[sys_recvfrom] get socket sockfd: {}", sockfd);
    log::info!("[sys_recvfrom] is nonblock:{:?}", is_nonblock);
    // 页表转换提到外面，避免 wait_io 循环中重复翻译
    let buf_slice = crate::trans_refmut!(buf, len);

    let mut recv = || match socket.socket_type() {
        SocketType::SOCK_STREAM | SocketType::SOCK_DGRAM | SocketType::SOCK_RAW => {
            let ret = socket.try_recv(buf_slice)?;
            if ret > 0 && src_addr != 0 {
                let _ = socket.peer_addr(src_addr, addrlen);
            }
            Ok(ret)
        }
        _ => todo!(),
    };
    if let Some(wait_queue) = socket.recv_wait_queue() {
        if is_nonblock {
            match recv() {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wait_queue, || match recv() {
                Ok(n) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(recv, is_nonblock)
    }
}
