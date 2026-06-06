use log::info;

use crate::mm::{copy_to_user_array, UserPtr};
use crate::net::config::NET_INTERFACE;
use crate::net::PSOCK;
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
    let len = (len as usize).min(64 * 1024 * 1024) as u32;
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let is_peek = msg_flags.contains(MsgFlags::MSG_PEEK);
    let msg_dontwait = match msg_flags.validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let socket = crate::get_socket!(sockfd);

    // 在 syscall 入口校验 src_addr 对应的 *addrlen 值
    if src_addr != 0 {
        let token = task.get_user_token();
        match UserPtr::<u32>::from_addr(addrlen).read(token) {
            Ok(len) => {
                // addrlen 过小（< sizeof(struct sockaddr_nl)=12）或过大（不合理）都返回 EINVAL
                if len < 12 || len > 512 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            Err(_) => return -(SyscallErr::EFAULT as isize),
        }
    }

    let is_nonblock = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msg_dontwait;

    info!("[sys_recvfrom] get socket sockfd: {}", sockfd);
    log::info!("[sys_recvfrom] is nonblock:{:?}", is_nonblock);

    // 使用 kernel buffer 中转，避免 trans_refmut! 跨页时返回非连续物理内存的 bug。
    // trans_refmut! 验证了所有用户页，但只返回第一页的切片指针，超出第一页边界
    // 的数据会写到错误地址。
    let token = task.get_user_token();
    let len_usize = len as usize;
    let (result, kernel_buf) = {
        let mut kernel_buf = alloc::vec![0u8; len_usize];

        let mut recv = || match socket.socket_type() {
            PSOCK::Stream => {
                // TCP (SOCK_STREAM): recvfrom behaves like recv, ignores from/addrlen
                let ret = socket.try_recv(&mut kernel_buf)?;
                Ok(ret)
            }
            PSOCK::Datagram | PSOCK::Raw => {
                let (ret, src_ep) = if is_peek {
                    socket.try_peek_recvmsg(&mut kernel_buf)?
                } else {
                    socket.try_recvmsg(&mut kernel_buf)?
                };
                log::info!("[sys_recvfrom] Datagram try_recvmsg returned {} bytes", ret);
                // 注意这里是 >= 0，因为 UDP 允许发送 0 字节的空包
                if ret >= 0 && src_addr != 0 {
                    if let Some(ep) = src_ep {
                        let _ = ep.fill_sockaddr(src_addr, addrlen);
                    }
                }
                Ok(ret)
            }
            _ => todo!(),
        };
        if let Some(wait_queue) = socket.recv_wait_queue() {
            if is_nonblock {
                // Non-blocking: poll once before trying to recv, so smoltcp can
                // advance TCP state (handshake, data delivery). Without this, a
                // tight non-blocking recv loop can starve the timer interrupt.
                NET_INTERFACE.try_poll();
                log::info!("[sys_recvfrom] after try_poll, calling recv()");
                let n = match recv() {
                    Ok(n) => n as isize,
                    Err(e) => -(e as isize),
                };
                (n, kernel_buf)
            } else {
                let n = WaitQueue::wait_until_interruptible(wait_queue, || match recv() {
                    Ok(n) => Some(n as isize),
                    Err(SyscallErr::EAGAIN) => {
                        log::debug!("[sys_recvfrom] EAGAIN, will sleep");
                        None
                    }
                    Err(e) => Some(-(e as isize)),
                })
                .unwrap_or_else(|e| e);
                (n, kernel_buf)
            }
        } else {
            (wait_io(recv, is_nonblock), kernel_buf)
        }
    };

    if result > 0 {
        if copy_to_user_array(
            token,
            kernel_buf.as_ptr(),
            buf as *mut u8,
            result as usize,
        )
        .is_err()
        {
            return -(SyscallErr::EFAULT as isize);
        }
    }
    result
}
