use crate::mm::{UserBufferWriter, UserPtrMut};
use crate::net::{TcpInfo, TCP_MSS};
use crate::task::current_task;
use crate::timer::TimeVal;
use crate::utils::error::SyscallErr;

use super::common::is_known_sockopt_level;
use super::common::{SOL_SOCKET, SOL_TCP, TCP_CONGESTION, TCP_INFO, TCP_MAXSEG};
use super::common::{SO_RCVBUF, SO_RCVTIMEO, SO_REUSEADDR, SO_SNDBUF, SO_SNDTIMEO};

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
    let optval_ptr = UserPtrMut::<u32>::from_addr(optval_ptr_);
    let optlen_ptr = UserPtrMut::<u32>::from_addr(optlen);
    match (level, optname) {
        (SOL_TCP, TCP_MAXSEG) => {
            // return max tcp fregment size (MSS)
            let len = core::mem::size_of::<u32>();
            if optval_ptr.write(token, &TCP_MSS).is_err()
                || optlen_ptr.write(token, &(len as u32)).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        (SOL_TCP, TCP_INFO) => {
            let state = socket.tcp_state().unwrap_or(7); // default Closed
            let info = TcpInfo::new(state, TCP_MSS);
            let info_len = core::mem::size_of::<TcpInfo>();
            let mut buf = match UserBufferWriter::new(token, optval_ptr_ as *mut u8, info_len) {
                Ok(writer) => writer,
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            let info_bytes = unsafe {
                core::slice::from_raw_parts(&info as *const TcpInfo as *const u8, info_len)
            };
            if buf.write_from(info_bytes).is_err()
                || optlen_ptr.write(token, &(info_len as u32)).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        (SOL_TCP, TCP_CONGESTION) => {
            let congestion = "reno";
            let mut optval_buf =
                match UserBufferWriter::new(token, optval_ptr_ as *mut u8, congestion.len()) {
                    Ok(writer) => writer,
                    Err(_) => return -(SyscallErr::EFAULT as isize),
                };
            if optval_buf.write_from(congestion.as_bytes()).is_err()
                || optlen_ptr.write(token, &(congestion.len() as u32)).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_REUSEADDR) => {
            // 对于需要写入 u32 的选项，检查 optlen 是否够大
            let optlen_val = match optlen_ptr.read(token) {
                Ok(len) => len,
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            if optlen_val < 4 {
                return -(SyscallErr::EINVAL as isize);
            }
            let socket = crate::get_socket!(sockfd);

            match optname {
                SO_SNDBUF => {
                    let size = socket.send_buf_size();
                    if optval_ptr.write(token, &(size as u32)).is_err()
                        || optlen_ptr.write(token, &4).is_err()
                    {
                        return -(SyscallErr::EFAULT as isize);
                    }
                }
                SO_RCVBUF => {
                    let size = socket.recv_buf_size();
                    if optval_ptr.write(token, &(size as u32)).is_err()
                        || optlen_ptr.write(token, &4).is_err()
                    {
                        return -(SyscallErr::EFAULT as isize);
                    }
                }
                SO_REUSEADDR => {
                    let enabled = match socket.reuse_addr() {
                        Ok(enabled) => enabled,
                        Err(e) => return -(e as isize),
                    };
                    if optval_ptr.write(token, &(enabled as u32)).is_err()
                        || optlen_ptr.write(token, &4).is_err()
                    {
                        return -(SyscallErr::EFAULT as isize);
                    }
                }
                _ => {
                    return -(SyscallErr::EINVAL as isize);
                }
            }
        }
        (SOL_SOCKET, SO_RCVTIMEO | SO_SNDTIMEO) => {
            let optlen_val = match optlen_ptr.read(token) {
                Ok(len) => len,
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            let len = core::mem::size_of::<TimeVal>();
            if optlen_val < len as u32 {
                return -(SyscallErr::EINVAL as isize);
            }
            let timeout = TimeVal::new();
            let mut writer = match UserBufferWriter::new(token, optval_ptr_ as *mut u8, len) {
                Ok(writer) => writer,
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            let bytes = unsafe {
                core::slice::from_raw_parts(&timeout as *const TimeVal as *const u8, len)
            };
            if writer.write_from(bytes).is_err() || optlen_ptr.write(token, &(len as u32)).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
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
