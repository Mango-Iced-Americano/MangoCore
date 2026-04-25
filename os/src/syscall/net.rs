use super::errno::*;
use crate::mm::{translated_ref, translated_refmut};
use crate::{
    config::PAGE_SIZE,
    fs::FileDescriptor,
    net::{
        address::{self, SocketAddrv4},
        make_unix_socket_pair, Socket, SocketType, TcpInfo, TCP_MSS,
    },
    task::current_task,
};

use crate::utils::error::SyscallErr;

use crate::syscall::utils::wait_io;
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
    let is_confilct = {
        let table = task.socket_table.lock();
        table.can_bind(endpoint, &socket).is_some()
    };
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

    wait_io(
        || socket.accept(sockfd, addr, addrlen).map(|s| s as isize),
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

    // 握手未完成，进入 wait_io 等待
    wait_io(|| socket.try_connect(), is_nonblock)
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
    _flags: u32,
    dest_addr: usize,
    addrlen: u32,
) -> isize {
    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    let buf = trans_ref!(buf, len);
    let socket = get_socket!(sockfd);
    log::info!("[sys_sendto] get socket sockfd: {}", sockfd);
    let is_nonblock = socket_file.get_nonblock() || (_flags & 0x40) != 0;

    match socket.socket_type() {
        SocketType::SOCK_DGRAM => {
            if socket.loacl_endpoint().port == 0 {
                let addr = SocketAddrv4::new([0; 16].as_slice());
                let endpoint = IpListenEndpoint::from(addr);
                let _ = socket.bind(endpoint);
            }
            let dest_addr = trans_ref!(dest_addr, addrlen);
            let _ = socket.connect(dest_addr);
            wait_io(|| socket.try_send(buf), is_nonblock)
        }
        SocketType::SOCK_STREAM => wait_io(|| socket.try_send(buf), is_nonblock),
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
    _flags: u32,
    src_addr: usize,
    addrlen: usize,
) -> isize {
    let task = current_task().unwrap();
    let socket_file = match task.files.lock().get_ref(sockfd as usize) {
        Ok(file) => file.clone(),
        Err(e) => return e,
    };
    //info!("[sys_recvfrom] file filags: {:?}", socket_file.flags);
    let socket = get_socket!(sockfd);

    let is_nonblock = {
        let fd_table = task.files.lock();
        fd_table
            .get_ref(sockfd as usize)
            .map(|fd| fd.get_nonblock())
            .unwrap_or(false)
    } || (_flags & 0x40) != 0;

    info!("[sys_recvfrom] get socket sockfd: {}", sockfd);
    log::info!("[sys_recvfrom] is nonblock:{:?}", is_nonblock);
    // 页表转换提到外面，避免 wait_io 循环中重复翻译
    let token = task.get_user_token();
    let buf_ptr = translated_refmut(token, buf as *mut u8).unwrap();
    let buf_slice = unsafe { core::slice::from_raw_parts_mut(buf_ptr, len as usize) };

    wait_io(
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
    let optval_ptr = translated_refmut(token, optval_ptr_ as *mut u32).unwrap();
    let optlen = translated_refmut(token, optlen as *mut u32).unwrap();
    match (level, optname) {
        (SOL_TCP, TCP_MAXSEG) => {
            // return max tcp fregment size (MSS)
            let len = core::mem::size_of::<u32>();
            unsafe {
                *(optval_ptr as *mut u32) = TCP_MSS;
                *(optlen as *mut u32) = len as u32;
            }
        }
        (SOL_TCP, TCP_INFO) => {
            let state = socket.tcp_state().unwrap_or(7); // default Closed
            let info = TcpInfo::new(state, TCP_MSS);
            let info_len = core::mem::size_of::<TcpInfo>();
            let buf = translated_refmut(token, optval_ptr_ as *mut u8).unwrap();
            let buf = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, info_len) };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &info as *const TcpInfo as *const u8,
                    buf.as_mut_ptr(),
                    info_len,
                );
            }
            unsafe {
                *(optlen as *mut u32) = info_len as u32;
            }
        }
        (SOL_TCP, TCP_CONGESTION) => {
            let optval_ptr = translated_refmut(token, optval_ptr_ as *mut u8).unwrap();
            let congestion = "reno";
            let buf =
                unsafe { core::slice::from_raw_parts_mut(optval_ptr as *mut u8, congestion.len()) };
            buf.copy_from_slice(congestion.as_bytes());
            unsafe {
                *(optlen as *mut u32) = congestion.len() as u32;
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
    let optval_ptr = translated_refmut(token, optval_ptr as *mut u32).unwrap();
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
    let fd1 = current_task()
        .unwrap()
        .files
        .lock()
        .insert(FileDescriptor::new(false, false, socket1));
    let fd2 = current_task()
        .unwrap()
        .files
        .lock()
        .insert(FileDescriptor::new(false, false, socket2));
    sv[0] = fd1.unwrap() as u32;
    sv[1] = fd2.unwrap() as u32;
    info!("[sys_socketpair] new sv: {:?}", sv);
    0 as isize
}
