use log::info;

use crate::mm::{copy_to_user_array, UserBufferWriter, UserPtr};
use crate::net::config::NET_INTERFACE;
use crate::net::PSOCK;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

/// 从 socket 接收数据并获取发送方地址。
///
/// # Semantics
///
/// 按 socket 类型（`Stream`/`Datagram`/`Raw`）分发：
/// - 校验 `MsgFlags`（`MSG_OOB` → `-EINVAL`，`MSG_ERRQUEUE` → `-EAGAIN`）。
/// - `MSG_PEEK`：委托 `try_peek_recvmsg()`（不消费数据），UDP socket 通过此标志
///   查看队列头部的数据。
/// - 非阻塞 `Stream` fast path：使用 `UserBufferWriter` 零拷贝接收，
///   在 `try_recv` 前等待一张内部 poll ticket，确保 CPU0 已完成一次有界扫描。
///   缺少此 poll 会导致非阻塞 recv 循环饿死定时器中断。
/// - 阻塞模式与 Datagram/Raw：先接收到内核 `Vec<u8>`，完成等待后再通过
///   `copy_to_user_array` 写回，避免跨越等待点保存用户页视图。
/// - `src_addr`/`addrlen`：接收前验证 `*addrlen` 值（负值或过小 → `-EINVAL`），
///   接收后通过 `Endpoint::fill_sockaddr()` 写回用户空间。
///
/// **关键约束**：非阻塞路径必须在 `try_recv` 前等待一张内部 poll ticket。
///
/// # Errors
///
/// - `-EFAULT`：用户缓冲区指针非法。
/// - `-EINVAL`：`*addrlen` 无效（`<12` 或 `>512`）。
/// 其他错误由 `Socket::try_recv`/`try_recvmsg` 产生。
///
/// # Linux Compatibility
///
/// TCP (`SOCK_STREAM`) 下 `recvfrom` 行为等同于 `recv` —— 忽略 `src_addr`/`addrlen`。
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

    let token = task.get_user_token();
    let len_usize = len as usize;

    // Stream non-blocking zero-copy fast path
    if is_nonblock && matches!(socket.socket_type(), PSOCK::Stream) {
        let ticket = NET_INTERFACE.request_poll_ticket();
        let _ = NET_INTERFACE.wait_poll_completion(ticket);
        let mut ubuf = match UserBufferWriter::new(token, buf as *mut u8, len_usize) {
            Ok(w) => w.into_user_buffer(),
            Err(e) => return e,
        };
        let n = match socket.try_recv_user(&mut ubuf, msg_flags) {
            Ok(n) => n,
            Err(e) => return -(e as isize),
        };
        return n;
    }

    // 阻塞 recv 会跨越 wait queue 等待点，因此数据先落到内核所有的 buffer。
    // 唤醒后再走统一 uaccess copy，不让用户映射的生命期与 socket 等待耦合。
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
                // `ret >= 0` 而非 `ret > 0`：UDP 允许发送 0 字节的数据报（空 payload）。
                if ret >= 0 && src_addr != 0 {
                    if let Some(ep) = src_ep {
                        let _ = ep.fill_sockaddr(src_addr, addrlen);
                    }
                }
                Ok(ret)
            }
            // TODO(socket-type-recv): unknown socket type in recvfrom fallback —
            // should return `-ESOCKTNOSUPPORT` once all socket types are covered.
            // Exit condition: `Socket::alloc()` only produces known types (Stream/Datagram/Raw).
            _ => Err(SyscallErr::EOPNOTSUPP),
        };
        if let Some(wait_queue) = socket.recv_wait_queue() {
            if is_nonblock {
                // 非阻塞首试在无 fd/socket/N2 锁时等待一张 ticket，不能把异步 kick
                // 当成同步 poll 结果；这段等待不消耗用户 I/O timeout。
                let ticket = NET_INTERFACE.request_poll_ticket();
                let _ = NET_INTERFACE.wait_poll_completion(ticket);
                log::info!("[sys_recvfrom] after poll ticket, calling recv()");
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
        if copy_to_user_array(token, kernel_buf.as_ptr(), buf as *mut u8, result as usize).is_err()
        {
            return -(SyscallErr::EFAULT as isize);
        }
    }
    result
}
