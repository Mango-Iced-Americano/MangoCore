use log::info;

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
    let msg_dontwait = match MsgFlags::from_bits_truncate(flags).validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let socket = crate::get_socket!(sockfd);

    // 在 syscall 入口校验 src_addr 对应的 *addrlen 值
    if src_addr != 0 {
        let token = task.get_user_token();
        match crate::mm::translated_ref(token, addrlen as *const u32) {
            Ok(addrlen_val) => {
                let len = *addrlen_val;
                // addrlen 过小（< sizeof(struct sockaddr_in)=16）或过大（不合理）都返回 EINVAL
                if len < 16 || len > 512 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            Err(_) => return -(SyscallErr::EFAULT as isize),
        }
    }

    let is_nonblock = {
        let fd_table = task.files.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msg_dontwait;

    info!("[sys_recvfrom] get socket sockfd: {}", sockfd);
    log::info!("[sys_recvfrom] is nonblock:{:?}", is_nonblock);
    // 页表转换提到外面，避免 wait_io 循环中重复翻译
    let buf_slice = crate::trans_refmut!(buf, len);

    let mut recv = || match socket.socket_type() {
        PSOCK::Stream => {
            // TCP (SOCK_STREAM): recvfrom behaves like recv, ignores from/addrlen
            let ret = socket.try_recv(buf_slice)?;
            Ok(ret)
        }
        PSOCK::Datagram | PSOCK::Raw => {
            let (ret, src_ep) = socket.try_recvmsg(buf_slice)?;
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
            match recv() {
                Ok(n) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wait_queue, || match recv() {
                Ok(n) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => {
                    log::debug!("[sys_recvfrom] EAGAIN, will sleep");
                    None
                }
                Err(e) => Some(-(e as isize)),
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(recv, is_nonblock)
    }
}
