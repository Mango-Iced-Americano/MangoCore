use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::check_addrlen;

pub fn sys_connect(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    match check_addrlen(addrlen) {
        Ok(_) => {}
        Err(e) => return -(e as isize),
    }
    let addr_buf = crate::trans_ref!(addr, addrlen);
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();

    let is_nonblock = task
        .files
        .lock()
        .get_ref(sockfd as usize)
        .map(|fd| fd.get_nonblock())
        .unwrap_or(false);

    // 先尝试初始化连接（只做一次）
    match socket.connect(addr_buf) {
        Ok(n) => return n as isize,
        Err(SyscallErr::EAGAIN) => {} // 需要 wait_io
        Err(e) => return -(e as isize),
    }

    // 握手未完成，进入等待队列等待状态变化
    if let Some(wait_queue) = socket.connect_wait_queue() {
        if is_nonblock {
            match socket.try_connect() {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wait_queue, || match socket.try_connect() {
                Ok(n) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(|| socket.try_connect(), is_nonblock)
    }
}
