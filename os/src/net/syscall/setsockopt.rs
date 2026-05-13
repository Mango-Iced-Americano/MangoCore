use crate::mm::get_from_user;
use crate::task::current_task;
use crate::utils::error::SyscallErr;

use super::common::{SOL_SOCKET, SOL_TCP, TCP_NODELAY};
use super::common::{SO_DONTROUTE, SO_KEEPALIVE, SO_RCVBUF, SO_REUSEADDR, SO_SNDBUF};

pub fn sys_setsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr: usize,
    optlen: u32,
) -> isize {
    let socket = crate::get_socket!(sockfd);

    // NULL 指针检查：optval_ptr == 0 时返回 EFAULT
    if optval_ptr == 0 {
        return -(SyscallErr::EFAULT as isize);
    }

    // optlen 为 0 时无法读取任何选项数据，返回 EINVAL
    if optlen == 0 {
        return -(SyscallErr::EINVAL as isize);
    }

    let task = current_task().unwrap();
    let token = task.get_user_token();
    let optval = match get_from_user::<u32>(token, optval_ptr as *const u32) {
        Ok(v) => v,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    match (level, optname) {
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => {
            let size = optval;
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
            let enabled = optval;
            log::debug!("[sys_setsockopt] set TCPNODELY: {}", enabled);
            let _ = match enabled {
                0 => socket.set_nagle_enabled(true),
                _ => socket.set_nagle_enabled(false),
            };
        }
        (SOL_SOCKET, SO_KEEPALIVE) => {
            let enabled = optval;
            log::debug!("[sys_setsockopt] set socket KEEPALIVE: {}", enabled);
            let _ = match enabled {
                1 => socket.set_keep_alive(true),
                _ => socket.set_keep_alive(false),
            };
        }
        (SOL_SOCKET, SO_REUSEADDR) => {
            let enabled = optval;
            log::debug!("[sys_setsockopt] set socket REUSEADDR: {}", enabled);
            let _ = match enabled {
                0 => socket.set_reuse_addr(false),
                _ => socket.set_reuse_addr(true),
            };
        }
        (SOL_SOCKET, SO_DONTROUTE) => {
            // do noting, just return success
            log::warn!("[sys_setsockopt] set socket DONTROUTE: {}", optval);
        }
        _ => {
            log::warn!(
                "[sys_setsockopt] level: {}, optname: {} not supported",
                level,
                optname
            );
            // Linux 语义：无论 level 是否已知，未知的 level/optname 组合都返回 ENOPROTOOPT
            return -(SyscallErr::ENOPROTOOPT as isize);
        }
    }
    0 as isize
}
