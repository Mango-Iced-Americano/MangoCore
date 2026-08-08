use crate::mm::{UserIoVec, UserPtr, UserPtrMut};
use crate::net::config::NET_INTERFACE;
use crate::net::posix::MsgHdr;
use crate::net::PSOCK;
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::WaitQueue;
use crate::utils::error::SyscallErr;

use super::common::MsgFlags;

/// 从 socket 接收消息并分散写入 `struct iovec` 数组。
///
/// # Semantics
///
/// 从 `msg_ptr` 指向的 `MsgHdr` 读取 `msg_iov`/`msg_iovlen` 描述的用户缓冲区，
/// 调用 `Socket::try_recvmsg()` 或 `try_peek_recvmsg()` 接收数据。
/// 填充 `msg_namelen`（发送方地址）、`msg_controllen`（控制信息，当前为 0）、
/// `msg_flags`（`MSG_TRUNC` 等返回值标志）。
///
/// # Errors
///
/// - `-EFAULT`：`msg_ptr` 或 `msg_iov` 指针非法。
/// - `-ENOBUFS`：内核临时缓冲区分配失败。
/// - `-EMSGSIZE`：用户 `iovec` 总长度不足以容纳消息时设置 `MSG_TRUNC`。
/// 其他错误由 `Socket::try_recvmsg()` 产生。
///
/// # Linux Compatibility
///
/// 当前不支持控制消息（`MSG_CTRUNC` 始终为 0），`msg_controllen` 返回 0。
/// `MSG_PEEK` 行为与 Linux 6.6 一致：查看数据但不消费。
pub fn sys_recvmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let is_peek = msg_flags.contains(MsgFlags::MSG_PEEK);
    let msgdontwait = match msg_flags.validate_for_recv() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let token = task.get_user_token();

    let is_nonblock = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msgdontwait;

    let msg = match UserPtr::<MsgHdr>::from_addr(msg_ptr).read(token) {
        Ok(m) => m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    let user_iov = match UserIoVec::read_user_iovecs(
        token,
        msg.msg_iov as *const crate::fs::iov::IOVec,
        msg.msg_iovlen,
        crate::hal::MAX_RW_COUNT,
    ) {
        Ok(iov) => iov,
        Err(errno) => return errno,
    };

    let recv_cap = user_iov.total_len().min(crate::hal::IO_CHUNK_SIZE);
    let socket = crate::get_socket!(sockfd);

    // Stream non-blocking zero-copy fast path
    if is_nonblock && !is_peek && matches!(socket.socket_type(), PSOCK::Stream) {
        let mut ubuf = match user_iov.writer_buffer_at(0, recv_cap) {
            Ok(b) => b,
            Err(errno) => return errno,
        };
        NET_INTERFACE.poll_now();
        let nbytes = match socket.try_recv_user(&mut ubuf, msg_flags) {
            Ok(n) if n >= 0 => n as usize,
            Ok(_) => 0usize,
            Err(e) => return -(e as isize),
        };

        let truncated = nbytes > recv_cap;

        if !msg.msg_name.is_null() && msg.msg_namelen >= 12 {
            if let Some(src_addr) = socket.last_recv_addr() {
                let namelen_field_offset = msg_ptr + core::mem::offset_of!(MsgHdr, msg_namelen);
                let _ = src_addr.fill_sockaddr(msg.msg_name as usize, namelen_field_offset);
            }
        }

        let msg_controllen = 0usize;
        if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_controllen))
            .write(token, &msg_controllen)
            .is_err()
        {
            return -(SyscallErr::EFAULT as isize);
        }
        let ret_flags: i32 = if truncated {
            (MsgFlags::MSG_TRUNC).bits() as i32
        } else {
            0i32
        };
        if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_flags))
            .write(token, &ret_flags)
            .is_err()
        {
            return -(SyscallErr::EFAULT as isize);
        }

        return nbytes as isize;
    }

    let mut buf = alloc::vec::Vec::new();
    if buf.try_reserve(recv_cap).is_err() {
        return -(SyscallErr::ENOBUFS as isize);
    }
    // Safety: `try_reserve(recv_cap)` succeeded above, so the allocation
    // has at least `recv_cap` bytes of capacity. `set_len` marks the whole
    // capacity as initialized — it will be overwritten by `try_recvmsg()`
    // before any read, so uninitialized bytes are never observed.
    unsafe {
        buf.set_len(recv_cap);
    }

    let mut try_recv = || {
        if is_peek {
            socket.try_peek_recvmsg_without_poll(&mut buf)
        } else {
            socket.try_recvmsg_without_poll(&mut buf)
        }
    };
    // Locking: `socket.recv_wait_queue()` 由 socket 接收路径唤醒（数据到达时
    // smoltcp 调用 `process()` → `recv_raw()` → `wake_one_if()`）。
    // `wait_until_interruptible` 的条件闭包仅调用 `try_recvmsg()`/`try_peek_recvmsg()`，
    // 不持有 socket 内部锁或 `NET_INTERFACE` 全局锁，不会导致锁顺序反转。
    let ret = if let Some(wq) = socket.recv_wait_queue() {
        if is_nonblock {
            NET_INTERFACE.poll_now();
            match try_recv() {
                Ok((n, _)) => n as isize,
                Err(e) => -(e as isize),
            }
        } else {
            WaitQueue::wait_until_interruptible(wq, || match try_recv() {
                Ok((n, _)) => Some(n as isize),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            })
            .unwrap_or_else(|e| e)
        }
    } else {
        wait_io(|| try_recv().map(|(n, _)| n), is_nonblock)
    };

    if ret < 0 {
        return ret;
    }
    let nbytes = ret as usize;
    let mut delivered = nbytes;

    let copy_len = nbytes.min(buf.len());
    if copy_len > 0 {
        let mut write_buf = match user_iov.writer_buffer_at(0, copy_len) {
            Ok(buffer) => buffer,
            Err(errno) => return errno,
        };
        delivered = match write_buf.write_from(&buf[..copy_len]) {
            Ok(copied) if copied == copy_len => nbytes,
            Ok(copied) => copied,
            Err(errno) => return errno,
        };
    }

    let truncated = nbytes > buf.len();

    if !msg.msg_name.is_null() && msg.msg_namelen >= 12 {
        if let Some(src_addr) = socket.last_recv_addr() {
            let namelen_field_offset = msg_ptr + core::mem::offset_of!(MsgHdr, msg_namelen);
            let _ = src_addr.fill_sockaddr(msg.msg_name as usize, namelen_field_offset);
        }
    }

    let msg_controllen = 0usize;
    if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_controllen))
        .write(token, &msg_controllen)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }
    let ret_flags: i32 = if truncated {
        (MsgFlags::MSG_TRUNC).bits() as i32
    } else {
        0i32
    };
    if UserPtrMut::from_addr(msg_ptr + core::mem::offset_of!(MsgHdr, msg_flags))
        .write(token, &ret_flags)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }

    delivered as isize
}
