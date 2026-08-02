use crate::mm::{UserBufferReader, UserIoVec, UserPtr};
use crate::net::config::NET_INTERFACE;
use crate::net::posix::MsgHdr;
use crate::net::{Endpoint, PSOCK};
use crate::syscall::utils::wait_io;
use crate::task::current_task;
use crate::task::{WaitQueue, WaitResult};
use crate::utils::error::SyscallErr;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

use super::common::MsgFlags;

/// 从用户空间的 `struct iovec` 数组收集数据并发送到 socket。
///
/// # Semantics
///
/// 从 `msg_ptr` 指向的 `MsgHdr` 读取 `msg_iov`/`msg_iovlen`/`msg_name`。
/// 按 socket 类型分发：
/// - `Stream`：分块发送（`send_stream_chunked`），逐 chunk 调用 `try_sendmsg`，
///   非阻塞路径通过 `UserBufferReader` 零拷贝，阻塞路径复制到内核 buf 后等待。
/// - `Datagram`/`Raw`：单次发送（`send_single_shot`），端口未绑定时自动分配临时端口。
///
/// # Errors
///
/// - `-EFAULT`：`msg_ptr` 或 `msg_iov` 指针非法。
/// - `-EDESTADDRREQ`：Datagram 类型无目标地址且未 `connect`。
/// - `-EMSGSIZE`：Datagram/Raw 数据超过 `IO_CHUNK_SIZE`。
/// - `-ENOBUFS`：内核临时缓冲区分配失败。
/// 其他错误由 `Socket::try_sendmsg()` 产生。
///
/// # Linux Compatibility
///
/// `MSG_OOB`（带外数据）对非 Stream socket 返回 `-EOPNOTSUPP`，与 Linux 6.6 一致。
/// 当前不支持 `MSG_EOR`（记录结束标记）。
pub fn sys_sendmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msg_flags = MsgFlags::from_bits_truncate(flags);
    let msgdontwait = match msg_flags.validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let is_nonblock = {
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        fd_table
            .get_file(sockfd as usize)
            .map(|f| f.is_nonblock())
            .unwrap_or(false)
    } || msgdontwait;

    let token = task.get_user_token();
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
    let total_len = user_iov.capped_len();
    if total_len == 0 {
        let socket = crate::get_socket!(sockfd);
        let dest_endpoint = resolve_dest(&msg, token, &*socket);
        return wait_io(
            || socket.try_sendmsg(&[], dest_endpoint.clone(), msg_flags),
            is_nonblock,
        );
    }

    let socket = crate::get_socket!(sockfd);
    let dest_endpoint = resolve_dest(&msg, token, &*socket);

    match socket.socket_type() {
        PSOCK::Stream => send_stream_chunked(
            &user_iov,
            total_len,
            &*socket,
            dest_endpoint,
            msg_flags,
            is_nonblock,
        ),
        PSOCK::Datagram => {
            if socket
                .local_endpoint()
                .map(|ep| ep.port() == 0)
                .unwrap_or(true)
            {
                let auto_bind = Endpoint::Ip(IpEndpoint::new(
                    IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
                    0,
                ));
                let _ = socket.bind(&auto_bind);
            }
            if dest_endpoint.is_none() && socket.remote_endpoint().is_none() {
                return -(SyscallErr::EDESTADDRREQ as isize);
            }
            if total_len > crate::hal::IO_CHUNK_SIZE {
                return -(SyscallErr::EMSGSIZE as isize);
            }
            send_single_shot(
                &user_iov,
                total_len,
                &*socket,
                dest_endpoint,
                msg_flags,
                is_nonblock,
            )
        }
        PSOCK::Raw => {
            if total_len > crate::hal::IO_CHUNK_SIZE {
                return -(SyscallErr::EMSGSIZE as isize);
            }
            send_single_shot(
                &user_iov,
                total_len,
                &*socket,
                dest_endpoint,
                msg_flags,
                is_nonblock,
            )
        }
        _ => {
            if total_len > crate::hal::IO_CHUNK_SIZE {
                return -(SyscallErr::EMSGSIZE as isize);
            }
            send_single_shot(
                &user_iov,
                total_len,
                &*socket,
                dest_endpoint,
                msg_flags,
                is_nonblock,
            )
        }
    }
}

fn resolve_dest(msg: &MsgHdr, token: usize, _socket: &crate::net::Socket) -> Option<Endpoint> {
    if msg.msg_name.is_null() || msg.msg_namelen < 16 {
        return None;
    }
    let copy_len = (msg.msg_namelen as usize).min(128);
    let addr_reader = match UserBufferReader::new(token, msg.msg_name as *const u8, copy_len) {
        Ok(reader) => reader,
        Err(_) => return None,
    };
    let mut addr_buf = [0u8; 128];
    if addr_reader.read_into(&mut addr_buf[..copy_len]).is_err() {
        return None;
    }
    Endpoint::from_sockaddr(&addr_buf[..copy_len]).ok()
}

fn send_stream_chunked(
    user_iov: &UserIoVec,
    total_len: usize,
    socket: &dyn crate::net::Socket,
    dest: Option<Endpoint>,
    msg_flags: MsgFlags,
    is_nonblock: bool,
) -> isize {
    let chunk_cap = total_len.min(crate::hal::IO_CHUNK_SIZE);

    if is_nonblock {
        // Zero-copy path: use UserBuffer directly, no kernel scratch buffer
        let mut done = 0usize;
        while done < total_len {
            let want = (total_len - done).min(chunk_cap);
            let accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
            if accessible == 0 {
                return if done > 0 {
                    done as isize
                } else {
                    -(SyscallErr::EFAULT as isize)
                };
            }

            let ubuf = match user_iov.reader_buffer_at(done, accessible) {
                Ok(b) => b,
                Err(errno) => return if done > 0 { done as isize } else { errno },
            };

            NET_INTERFACE.try_poll();
            let sent = match socket.try_sendmsg_user(&ubuf, dest.clone(), msg_flags) {
                Ok(n) => {
                    if n <= 0 {
                        return if done > 0 { done as isize } else { n };
                    }
                    n
                }
                Err(e) => {
                    return if done > 0 {
                        done as isize
                    } else {
                        -(e as isize)
                    };
                }
            };

            done += sent as usize;

            if (sent as usize) < accessible || accessible < want {
                break;
            }

            if let Some(task) = current_task() {
                if crate::task::has_actionable_signal(&task) {
                    break;
                }
            }
        }
        return done as isize;
    }

    // Blocking path: use kernel scratch buffer
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(chunk_cap).is_err() {
        return -(SyscallErr::ENOBUFS as isize);
    }
    // Safety: `try_reserve(chunk_cap)` succeeded above, ensuring capacity.
    // `set_len` marks the buffer as initialized — it will be fully overwritten
    // by `ubuf.read()` below before any read, so uninitialized access is impossible.
    unsafe {
        kbuf.set_len(chunk_cap);
    }

    let mut done = 0usize;
    while done < total_len {
        let want = (total_len - done).min(chunk_cap);
        let accessible = user_iov.accessible_len_at(done, want, crate::mm::UserAccess::Read);
        if accessible == 0 {
            return if done > 0 {
                done as isize
            } else {
                -(SyscallErr::EFAULT as isize)
            };
        }

        let ubuf = match user_iov.reader_buffer_at(done, accessible) {
            Ok(b) => b,
            Err(errno) => return if done > 0 { done as isize } else { errno },
        };
        let copied = ubuf.read(&mut kbuf[..accessible]);

        let send_fn = || {
            socket.try_sendmsg_without_poll(
                &kbuf[..copied.min(accessible)],
                dest.clone(),
                msg_flags,
            )
        };

        // Locking: `socket.send_wait_queue()` 由 socket 发送路径唤醒（发送缓冲区
        // 腾出空间时 smoltcp 调用 `dispatch()` → `wake_one_if()`）。
        // `wait_until_interruptible` 的条件闭包仅调用 `send_fn()`（即 `try_sendmsg()`），
        // 不持有 socket 内部锁或 `NET_INTERFACE` 全局锁，不会导致锁顺序反转。
        let sent: isize = if let Some(wq) = socket.send_wait_queue() {
            NET_INTERFACE.poll();
            let result = WaitQueue::wait_until_interruptible(wq, || match send_fn() {
                Ok(n) => Some(n),
                Err(SyscallErr::EAGAIN) => None,
                Err(e) => Some(-(e as isize)),
            });
            match result {
                WaitResult::Ready(n) => {
                    if n < 0 {
                        return if done > 0 { done as isize } else { n };
                    }
                    n
                }
                WaitResult::Interrupted => {
                    return if done > 0 {
                        done as isize
                    } else {
                        crate::task::RestartKind::RestartSys.syscall_result()
                    };
                }
                WaitResult::TimedOut => {
                    return if done > 0 {
                        done as isize
                    } else {
                        -(SyscallErr::EAGAIN as isize)
                    };
                }
            }
        } else {
            match send_fn() {
                Ok(n) => {
                    if n <= 0 {
                        return if done > 0 { done as isize } else { n };
                    }
                    n
                }
                Err(e) => {
                    return if done > 0 {
                        done as isize
                    } else {
                        -(e as isize)
                    };
                }
            }
        };

        done += sent as usize;

        if (sent as usize) < accessible || accessible < want {
            break;
        }

        if let Some(task) = current_task() {
            if crate::task::has_actionable_signal(&task) {
                break;
            }
        }
    }
    done as isize
}

fn send_single_shot(
    user_iov: &UserIoVec,
    total_len: usize,
    socket: &dyn crate::net::Socket,
    dest: Option<Endpoint>,
    msg_flags: MsgFlags,
    is_nonblock: bool,
) -> isize {
    let mut kbuf = alloc::vec::Vec::new();
    if kbuf.try_reserve(total_len).is_err() {
        return -(SyscallErr::ENOBUFS as isize);
    }
    // Safety: `try_reserve(total_len)` succeeded above, ensuring capacity.
    // `set_len` marks the buffer as initialized — it is fully overwritten
    // by `ubuf.read()` before any consumer, so uninitialized bytes are never observed.
    unsafe {
        kbuf.set_len(total_len);
    }

    let ubuf = match user_iov.reader_buffer_at(0, total_len) {
        Ok(b) => b,
        Err(errno) => return errno,
    };
    ubuf.read(&mut kbuf);

    wait_io(
        || socket.try_sendmsg(&kbuf, dest.clone(), msg_flags),
        is_nonblock,
    )
}
