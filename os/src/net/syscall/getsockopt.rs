use crate::mm::translated_refmut;
use crate::net::{TcpInfo, TCP_MSS};
use crate::task::current_task;
use crate::utils::error::SyscallErr;

use super::common::{SOL_SOCKET, SOL_TCP, SOL_IP, TCP_CONGESTION, TCP_INFO, TCP_MAXSEG};
use super::common::{SO_RCVBUF, SO_REUSEADDR, SO_SNDBUF};
use super::common::is_known_sockopt_level;

pub fn sys_getsockopt(
    sockfd: u32,
    level: u32,
    optname: u32,
    optval_ptr_: usize,
    optlen: usize,
) -> isize {
    let socket = crate::get_socket!(sockfd); // 检查socket存不存在

    // NULL 指针检查：optval_ptr_ == 0 或 optlen == 0 时返回 EFAULT
    if optval_ptr_ == 0 || optlen == 0 {
        return -(SyscallErr::EFAULT as isize);
    }

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
            // 对于需要写入 u32 的选项，检查 optlen 是否够大
            let optlen_val = unsafe { *optlen };
            if optlen_val < 4 {
                return -(SyscallErr::EINVAL as isize);
            }
            let socket = crate::get_socket!(sockfd);

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
            if is_known_sockopt_level(level) {
                return -(SyscallErr::ENOPROTOOPT as isize);
            } else {
                return -(SyscallErr::EOPNOTSUPP as isize);
            }
        }
    }
    0 as isize
}
