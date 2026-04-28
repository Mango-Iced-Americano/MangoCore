use super::errno::*;
use crate::fs::iov::IOVec;
use crate::mm::{
    copy_from_user_array, translated_byte_buffer_append_to_existing_vec, translated_ref,
    translated_refmut, UserBuffer,
};
use crate::{
    config::PAGE_SIZE,
    fs::FileDescriptor,
    net::{
        address::{self, SocketAddrv4},
        make_unix_socket_pair,
        posix::MsgHdr,
        Socket, SocketFile, SocketType, TcpInfo, TCP_MSS,
    },
    task::current_task,
};

use crate::utils::error::SyscallErr;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::syscall::utils::{wait_io, wait_socket_io};
use log::info;
use smoltcp::wire::IpListenEndpoint;

/// level
const SOL_SOCKET: u32 = 1;
const SOL_TCP: u32 = 6;
/// option name
const TCP_NODELAY: u32 = 1;
const TCP_MAXSEG: u32 = 2;
#[allow(unused)]
const TCP_INFO: u32 = 11;
const TCP_CONGESTION: u32 = 13;

const SO_DEBUG: u32 = 1;
const SO_REUSEADDR: u32 = 2;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_DONTROUTE: u32 = 5;
const SO_BROADCAST: u32 = 6;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const SO_KEEPALIVE: u32 = 9;
const SO_OOBINLINE: u32 = 10;
const SO_REUSEPORT: u32 = 15;

bitflags! {
    /// MSG flags for send/recv syscalls.
    pub struct MsgFlags: u32 {
        const MSG_OOB       = 0x0001;
        const MSG_PEEK      = 0x0002;
        const MSG_DONTROUTE = 0x0004;
        const MSG_CTRUNC    = 0x0008;
        const MSG_PROXY     = 0x0010;
        const MSG_TRUNC     = 0x0020;
        const MSG_DONTWAIT  = 0x0040;
        const MSG_EOR       = 0x0080;
        const MSG_WAITALL   = 0x0100;
        const MSG_FIN       = 0x0200;
        const MSG_SYN       = 0x0400;
        const MSG_CONFIRM   = 0x0800;
        const MSG_RST       = 0x1000;
        const MSG_ERRQUEUE  = 0x2000;
        const MSG_NOSIGNAL  = 0x4000;
        const MSG_MORE      = 0x8000;
    }
}

impl MsgFlags {
    /// Validate flags for recv syscalls (recvfrom, recvmsg, etc.).
    ///
    /// Returns `Ok(is_nonblock)` if flags are acceptable, or `Err(errno)`
    /// when an unsupported flag is set (e.g. `MSG_OOB`, `MSG_ERRQUEUE`).
    pub fn validate_for_recv(self) -> Result<bool, SyscallErr> {
        match () {
            _ if self.contains(MsgFlags::MSG_OOB) => Err(SyscallErr::EINVAL),
            _ if self.contains(MsgFlags::MSG_ERRQUEUE) => Err(SyscallErr::EAGAIN),
            _ => Ok(self.contains(MsgFlags::MSG_DONTWAIT)),
        }
    }

    /// Validate flags for send syscalls (sendto, sendmsg, etc.).
    pub fn validate_for_send(self) -> Result<bool, SyscallErr> {
        match () {
            _ if self.contains(MsgFlags::MSG_OOB) => Err(SyscallErr::EOPNOTSUPP),
            _ if self.contains(MsgFlags::MSG_ERRQUEUE) => Err(SyscallErr::EOPNOTSUPP),
            _ => Ok(self.contains(MsgFlags::MSG_DONTWAIT)),
        }
    }
}

pub fn sys_socket(domain: u32, socket_type: u32, protocol: u32) -> isize {
    info!(
        "[sys_socket] domain: {}, type: {}, protocol: {}",
        domain, socket_type, protocol
    );
    let result = match <dyn Socket>::alloc(domain, socket_type, protocol) {
        Ok(sockfd) => {
            info!("[sys_socket] new sockfd: {}", sockfd);
            sockfd as isize
        }
        Err(e) => {
            info!("[sys_socket] new sockfd failed",);
            -(e as isize)
        }
    };
    result
}

pub fn sys_bind(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    let addr_buf = trans_ref!(addr, addrlen);
    let socket = get_socket!(sockfd);
    let endpoint = match address::listen_endpoint(addr_buf) {
        Ok(ep) => ep,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let is_confilct = crate::net::check_port_conflict(&task, endpoint, &socket);
    if is_confilct {
        log::warn!("[sys_bind] port {} already in use", endpoint.port);
        return -(SyscallErr::EADDRINUSE as isize);
    }
    match socket.bind(endpoint) {
        Ok(_) => 0 as isize,
        Err(e) => -(e as isize),
    }
}

pub fn sys_listen(sockfd: u32, _backlog: u32) -> isize {
    let socket = get_socket!(sockfd);
    //socket.listen().unwrap() as isize
    match socket.listen() {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}

pub fn sys_accept(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = get_socket!(sockfd);
    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    let is_nonblock = socket_file.get_nonblock();

    wait_socket_io(
        || socket.accept(sockfd, addr, addrlen).map(|s| s as isize),
        socket.accept_wait_queue(),
        is_nonblock,
    )
}

pub fn sys_connect(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    let addr_buf = trans_ref!(addr, addrlen);
    let socket = get_socket!(sockfd);
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

    // 握手未完成，进入 wait_socket_io 等待
    wait_socket_io(
        || socket.try_connect(),
        socket.connect_wait_queue(),
        is_nonblock,
    )
}

pub fn sys_getsockname(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = get_socket!(sockfd);
    // socket.addr(addr, addrlen).unwrap() as isize
    match socket.addr(addr, addrlen) {
        Ok(new_fd) => new_fd as isize,
        Err(err) => -(err as isize),
    }
}

pub fn sys_getpeername(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = get_socket!(sockfd);
    // socket.peer_addr(addr, addrlen).unwrap() as isize
    match socket.peer_addr(addr, addrlen) {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}

pub fn sys_sendto(
    sockfd: u32,
    buf: usize,
    len: usize,
    flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize {
    let msg_dontwait = match MsgFlags::from_bits_truncate(flags).validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };

    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    let buf = trans_ref!(buf, len);
    let socket = get_socket!(sockfd);
    log::info!("[sys_sendto] get socket sockfd: {}", sockfd);
    let is_nonblock = socket_file.get_nonblock() || msg_dontwait;

    // Validate dest_addr/addrlen for connection-mode sockets
    if dest_addr != 0 {
        match socket.socket_type() {
            SocketType::SOCK_STREAM => {
                // Linux: sendto on a SOCK_STREAM with non-NULL dest_addr returns EISCONN
                return -(SyscallErr::EISCONN as isize);
            }
            SocketType::SOCK_DGRAM => {
                // Validate addrlen: must be at least sizeof(sockaddr_in) = 16, at most 128
                if addrlen < 16 || addrlen > 128 {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
            _ => {}
        }
    }

    match socket.socket_type() {
        SocketType::SOCK_DGRAM => {
            if socket.local_endpoint().port == 0 {
                let addr = SocketAddrv4::new([0; 16].as_slice());
                let endpoint = IpListenEndpoint::from(addr);
                let _ = socket.bind(endpoint);
            }
            let dest_addr = trans_ref!(dest_addr, addrlen);
            let _ = socket.connect(dest_addr);
            wait_io(|| socket.try_send(buf), is_nonblock)
        }
        SocketType::SOCK_STREAM => wait_socket_io(
            || socket.try_send(buf),
            socket.send_wait_queue(),
            is_nonblock,
        ),
        SocketType::SOCK_RAW => {
            info!("[sys_sendto] socket is raw");
            let dest_addr = trans_ref!(dest_addr, addrlen);
            let endpoint = address::endpoint(dest_addr).unwrap();
            wait_io(
                || socket.send_to(buf, endpoint).map(|n| n as isize),
                is_nonblock,
            )
        }
        _ => todo!(),
    }
}

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
    //info!("[sys_recvfrom] file filags: {:?}", socket_file.flags);
    let socket = get_socket!(sockfd);

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
    let buf_slice = trans_refmut!(buf, len);

    wait_socket_io(
        || match socket.socket_type() {
            SocketType::SOCK_STREAM | SocketType::SOCK_DGRAM | SocketType::SOCK_RAW => {
                let ret = socket.try_recv(buf_slice)?;
                if ret > 0 && src_addr != 0 {
                    let _ = socket.peer_addr(src_addr, addrlen);
                }
                Ok(ret)
            }
            _ => todo!(),
        },
        socket.recv_wait_queue(),
        is_nonblock,
    )
}

pub fn sys_getsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr_: usize,
    optlen: usize,
) -> isize {
    let socket = get_socket!(sockfd); // 检查socket存不存在
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let optval_ptr = match translated_refmut(token, optval_ptr_ as *mut u32) {
        Ok(p) => p as *mut u32,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    let optlen = match translated_refmut(token, optlen as *mut u32) {
        Ok(p) => p as *mut u32,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    match (level, optname) {
        (SOL_TCP, TCP_MAXSEG) => {
            // return max tcp fregment size (MSS)
            let len = core::mem::size_of::<u32>();
            unsafe {
                *optval_ptr = TCP_MSS;
                *optlen = len as u32;
            }
        }
        (SOL_TCP, TCP_INFO) => {
            let state = socket.tcp_state().unwrap_or(7); // default Closed
            let info = TcpInfo::new(state, TCP_MSS);
            let info_len = core::mem::size_of::<TcpInfo>();
            let buf = match translated_refmut(token, optval_ptr_ as *mut u8) {
                Ok(p) => unsafe { core::slice::from_raw_parts_mut(p as *mut u8, info_len) },
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &info as *const TcpInfo as *const u8,
                    buf.as_mut_ptr(),
                    info_len,
                );
                *optlen = info_len as u32;
            }
        }
        (SOL_TCP, TCP_CONGESTION) => {
            let congestion = "reno";
            let optval_ptr = match translated_refmut(token, optval_ptr_ as *mut u8) {
                Ok(p) => unsafe { core::slice::from_raw_parts_mut(p as *mut u8, congestion.len()) },
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            optval_ptr.copy_from_slice(congestion.as_bytes());
            unsafe {
                *optlen = congestion.len() as u32;
            }
        }
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_REUSEADDR) => {
            // let len = core::mem::size_of::<u32>();
            let socket = get_socket!(sockfd);

            match optname {
                SO_SNDBUF => {
                    let size = socket.send_buf_size();
                    unsafe {
                        *(optval_ptr as *mut u32) = size as u32;
                        *(optlen as *mut u32) = 4;
                    }
                }
                SO_RCVBUF => {
                    let size = socket.recv_buf_size();
                    unsafe {
                        *(optval_ptr as *mut u32) = size as u32;
                        *(optlen as *mut u32) = 4;
                    }
                }
                SO_REUSEADDR => {
                    let enabled = match socket.reuse_addr() {
                        Ok(enabled) => enabled,
                        Err(e) => return -(e as isize),
                    };
                    unsafe {
                        *(optval_ptr as *mut u32) = enabled as u32;
                        *(optlen as *mut u32) = 4;
                    }
                }
                _ => {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
        }
        _ => {
            log::warn!("[sys_getsockopt] level: {}, optname: {}", level, optname);
            return -(SyscallErr::ENOPROTOOPT as isize);
        }
    }
    0 as isize
}

pub fn sys_setsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr: usize,
    _optlen: u32,
) -> isize {
    let socket = get_socket!(sockfd);
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let optval_ptr = match translated_refmut(token, optval_ptr as *mut u32) {
        Ok(p) => p as *mut u32,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    match (level, optname) {
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => {
            let size = unsafe { *(optval_ptr as *mut u32) };
            match optname {
                SO_SNDBUF => {
                    socket.set_send_buf_size(size as usize);
                }
                SO_RCVBUF => {
                    socket.set_recv_buf_size(size as usize);
                }
                _ => {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
        }
        (SOL_TCP, TCP_NODELAY) => {
            // close Nagle’s Algorithm
            let enabled = unsafe { *(optval_ptr as *const u32) };
            log::debug!("[sys_setsockopt] set TCPNODELY: {}", enabled);
            let _ = match enabled {
                0 => socket.set_nagle_enabled(true),
                _ => socket.set_nagle_enabled(false),
            };
        }
        (SOL_SOCKET, SO_KEEPALIVE) => {
            let enabled = unsafe { *(optval_ptr as *const u32) };
            log::debug!("[sys_setsockopt] set socket KEEPALIVE: {}", enabled);
            let _ = match enabled {
                1 => socket.set_keep_alive(true),
                _ => socket.set_keep_alive(false),
            };
        }
        (SOL_SOCKET, SO_REUSEADDR) => {
            let enabled = unsafe { *(optval_ptr as *const u32) };
            log::debug!("[sys_setsockopt] set socket REUSEADDR: {}", enabled);
            let _ = match enabled {
                0 => socket.set_reuse_addr(false),
                _ => socket.set_reuse_addr(true),
            };
        }
        (SOL_SOCKET, SO_DONTROUTE) => {
            // do noting, just return success
            log::warn!("[sys_setsockopt] set socket DONTROUTE: {}", unsafe {
                *(optval_ptr as *const u32)
            });
        }
        _ => {
            log::warn!(
                "[sys_setsockopt] level: {}, optname: {} not supported",
                level,
                optname
            );
            return -(SyscallErr::ENOPROTOOPT as isize);
        }
    }
    0 as isize
}

pub fn sys_sock_shutdown(sockfd: u32, how: u32) -> isize {
    log::info!("[sys_shutdown] sockfd {}, how {}", sockfd, how);
    let socket = get_socket!(sockfd);
    let ret = socket.shutdown(how);
    match ret {
        Ok(_) => 0 as isize,
        Err(errno) => -(errno as isize),
    }
}

pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize {
    info!(
        "[sys_socketpair] domain {}, type {}, protocol {}, sv {}",
        domain, socket_type, protocol, sv
    );
    let len = 2 * core::mem::size_of::<u32>();
    let sv = unsafe { core::slice::from_raw_parts_mut(sv as *mut u32, len) };
    let (socket1, socket2) = make_unix_socket_pair::<PAGE_SIZE>();
    let socket_file1 = Arc::new(SocketFile::new(socket1));
    let socket_file2 = Arc::new(SocketFile::new(socket2));
    let fd1 = current_task()
        .unwrap()
        .files
        .lock()
        .insert(FileDescriptor::new(false, false, socket_file1));
    let fd2 = current_task()
        .unwrap()
        .files
        .lock()
        .insert(FileDescriptor::new(false, false, socket_file2));
    sv[0] = fd1.unwrap() as u32;
    sv[1] = fd2.unwrap() as u32;
    info!("[sys_socketpair] new sv: {:?}", sv);
    0 as isize
}

pub fn sys_sendmsg(sockfd: u32, msg_ptr: usize, flags: u32) -> isize {
    let msgdontwait = match MsgFlags::from_bits_truncate(flags).validate_for_send() {
        Ok(nb) => nb,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(f) => f.clone(),
        Err(e) => return e,
    };
    let is_nonblock = socket_file.get_nonblock() || msgdontwait;

    let token = task.get_user_token();
    let msg = match translated_ref(token, msg_ptr as *const MsgHdr) {
        Ok(m) => *m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    // 读取 iovec 数组
    let iov_cnt = msg.msg_iovlen;
    let mut iovecs = alloc::vec![IOVec {iov_base: core::ptr::null(), iov_len: 0}; iov_cnt];
    if copy_from_user_array(token, msg.msg_iov, iovecs.as_mut_ptr(), iov_cnt).is_err() {
        return -(SyscallErr::EFAULT as isize);
    }

    // 从用户 iovec 读取数据到内核 flat buffer
    let total_len: usize = iovecs.iter().map(|iov| iov.iov_len).sum();
    let mut buf_parts = Vec::new();
    for iov in &iovecs {
        if iov.iov_len == 0 {
            continue;
        }
        match translated_byte_buffer_append_to_existing_vec(
            &mut buf_parts,
            token,
            iov.iov_base,
            iov.iov_len,
        ) {
            Ok(_) => {}
            Err(e) => return e,
        }
    }
    let mut buf = alloc::vec![0u8; total_len];
    {
        let user_buf = UserBuffer::new(buf_parts);
        user_buf.read(&mut buf);
    }

    // 解析目标地址（msg_name）
    let dest_addr = if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        let copy_len = (msg.msg_namelen as usize).min(128);
        let mut addr_parts = Vec::new();
        match translated_byte_buffer_append_to_existing_vec(
            &mut addr_parts,
            token,
            msg.msg_name,
            copy_len,
        ) {
            Ok(_) => {
                let mut addr_buf = [0u8; 128];
                let addr_user_buf = UserBuffer::new(addr_parts);
                addr_user_buf.read(&mut addr_buf[..copy_len]);
                match address::endpoint(&addr_buf[..copy_len]) {
                    Ok(ep) => Some(ep),
                    Err(_) => return -(SyscallErr::EINVAL as isize),
                }
            }
            Err(e) => return e,
        }
    } else {
        None
    };

    let socket = get_socket!(sockfd);
    match socket.socket_type() {
        SocketType::SOCK_DGRAM => wait_io(|| socket.try_sendmsg(&buf, dest_addr), is_nonblock),
        SocketType::SOCK_STREAM => wait_socket_io(
            || socket.try_sendmsg(&buf, None),
            socket.send_wait_queue(),
            is_nonblock,
        ),
        SocketType::SOCK_RAW => wait_io(|| socket.try_sendmsg(&buf, dest_addr), is_nonblock),
        _ => wait_socket_io(
            || socket.try_sendmsg(&buf, dest_addr),
            socket.send_wait_queue(),
            is_nonblock,
        ),
    }
}

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
    let msg = match translated_ref(token, msg_ptr as *const MsgHdr) {
        Ok(m) => *m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };

    // 读取 iovec 数组
    let iov_cnt = msg.msg_iovlen;
    let mut iovecs = alloc::vec![IOVec {iov_base: core::ptr::null(), iov_len: 0}; iov_cnt];
    if copy_from_user_array(token, msg.msg_iov, iovecs.as_mut_ptr(), iov_cnt).is_err() {
        return -(SyscallErr::EFAULT as isize);
    }

    // 分配接收缓冲区
    let total_len: usize = iovecs.iter().map(|iov| iov.iov_len).sum();
    let mut buf = alloc::vec![0u8; total_len];

    let socket = get_socket!(sockfd);
    let ret = wait_socket_io(
        || socket.try_recvmsg(&mut buf).map(|(n, _)| n),
        socket.recv_wait_queue(),
        is_nonblock,
    );

    if ret < 0 {
        return ret;
    }
    let nbytes = ret as usize;

    // 将接收到的数据分散写入用户 iovec
    let mut write_parts = Vec::new();
    for iov in &iovecs {
        if iov.iov_len == 0 {
            continue;
        }
        match translated_byte_buffer_append_to_existing_vec(
            &mut write_parts,
            token,
            iov.iov_base,
            iov.iov_len,
        ) {
            Ok(_) => {}
            Err(e) => return e,
        }
    }
    {
        let mut write_buf = UserBuffer::new(write_parts);
        write_buf.write(&buf[..nbytes]);
    }

    // 写回源地址（msg_name）
    if !msg.msg_name.is_null() && msg.msg_namelen >= 16 {
        if let Some(src_addr) = socket.last_recv_addr() {
            let namelen_field_offset = msg_ptr + core::mem::offset_of!(MsgHdr, msg_namelen);
            let _ =
                address::fill_with_endpoint(src_addr, msg.msg_name as usize, namelen_field_offset);
        }
    }

    // 写回 msg_controllen = 0, msg_flags = 0
    let write_back = match translated_refmut(token, msg_ptr as *mut MsgHdr) {
        Ok(m) => m,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    write_back.msg_controllen = 0;
    write_back.msg_flags = 0;

    nbytes as isize
}
