use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

pub fn sys_accept(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    let task = current_task().unwrap();
    let is_nonblock = match task.process.files().lock().get_file(sockfd as usize) {
        Ok(f) => f.is_nonblock(),
        Err(e) => return -(e as isize),
    };

    if let Some(wait_queue) = socket.accept_wait_queue() {
        if is_nonblock {
            match socket.accept(sockfd, addr, addrlen) {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wait_queue, || {
                match socket.accept(sockfd, addr, addrlen) {
                    Ok(n) => Some(n as isize),
                    Err(SyscallErr::EAGAIN) => None,
                    Err(e) => Some(-(e as isize)),
                }
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(
            || socket.accept(sockfd, addr, addrlen).map(|s| s as isize),
            is_nonblock,
        )
    }
}

/// accept4(fd, addr, addrlen, flags)
///
/// `flags` can include `SOCK_CLOEXEC` and/or `SOCK_NONBLOCK`.
pub fn sys_accept4(sockfd: u32, addr: usize, addrlen: usize, flags: u32) -> isize {
    const SOCK_CLOEXEC: u32 = 1 << 19;
    const SOCK_NONBLOCK: u32 = 0x800;

    crate::trace_event!(0xB040, sockfd as u64, flags as u64, 0, 0, 0, 0);

    let ret = sys_accept(sockfd, addr, addrlen);

    crate::trace_event!(0xB041, ret as u64, 0, 0, 0, 0, 0);

    if ret < 0 {
        return ret;
    }
    let new_fd = ret as usize;

    let task = current_task().unwrap();
    let files_ref = task.process.files();
    let mut fd_table = files_ref.lock();
    if flags & SOCK_CLOEXEC != 0 {
        let _ = fd_table.set_cloexec(new_fd, true);
    }
    if flags & SOCK_NONBLOCK != 0 {
        if let Ok(f) = fd_table.get_file(new_fd) {
            f.set_nonblock(true);
        }
    }

    ret
}
