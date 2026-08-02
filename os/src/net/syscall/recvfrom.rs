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
///   在 `try_recv` 前调用 `NET_INTERFACE.try_poll()` 推进 TCP 状态机。
///   缺少此 poll 会导致非阻塞 recv 循环饿死定时器中断。
/// - 阻塞模式与 Datagram/Raw：复制到内核 `Vec<u8>` buf 中转
///   （`HACK(uaccess-contiguity)`：绕过 `trans_refmut!` 跨页连续性的已知 bug，
///   详见函数体内 HACK 注释）。
/// - `src_addr`/`addrlen`：接收前验证 `*addrlen` 值（负值或过小 → `-EINVAL`），
///   接收后通过 `Endpoint::fill_sockaddr()` 写回用户空间。
///
/// **关键约束**：非阻塞路径必须在 `try_recv` 前调用 `NET_INTERFACE.try_poll()`。
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
/// 内核 buf 中转方案是绕过 `trans_refmut!` 跨页 bug 的 workaround。
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
        NET_INTERFACE.try_poll();
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

    // HACK(uaccess-contiguity): 使用 kernel buffer 中转，避免 `trans_refmut!` 跨页时
    // 返回非连续物理内存的 bug。`trans_refmut!` 验证了所有用户页，但只返回第一页的
    // 切片指针，超出第一页边界的数据会写到错误地址。
    // Reference: `trans_refmut!` macro in `os/src/mm/uaccess.rs` — maps each page
    //   independently but returns only the first page's virtual address.
    // Remove when: `UserBufferWriter` supports scatter-gather writes across
    //   non-contiguous physical pages, allowing zero-copy recv even for cross-page buffers.
    let (result, kernel_buf) = {
        let mut kernel_buf = alloc::vec![0u8; len_usize];

        let mut recv = || match socket.socket_type() {
            PSOCK::Stream => {
                // TCP (SOCK_STREAM): recvfrom behaves like recv, ignores from/addrlen
                let ret = socket.try_recv_without_poll(&mut kernel_buf)?;
                Ok(ret)
            }
            PSOCK::Datagram | PSOCK::Raw => {
                let (ret, src_ep) = if is_peek {
                    socket.try_peek_recvmsg_without_poll(&mut kernel_buf)?
                } else {
                    socket.try_recvmsg_without_poll(&mut kernel_buf)?
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
                NET_INTERFACE.poll();
                let n = match WaitQueue::wait_until_interruptible(wait_queue, || match recv() {
                    Ok(n) => Some(n as isize),
                    Err(SyscallErr::EAGAIN) => {
                        log::debug!("[sys_recvfrom] EAGAIN, will sleep");
                        None
                    }
                    Err(e) => Some(-(e as isize)),
                }) {
                    crate::task::WaitResult::Ready(value) => value,
                    crate::task::WaitResult::Interrupted => {
                        crate::task::RestartKind::RestartSys.syscall_result()
                    }
                    crate::task::WaitResult::TimedOut => -(SyscallErr::EAGAIN as isize),
                };
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
