use crate::mm::{UserBufferWriter, UserPtrMut};
use crate::net::{TcpInfo, PSOCK, TCP_MSS};
use crate::task::current_task;
use crate::timer::TimeVal;
use crate::utils::error::SyscallErr;

use super::common::is_known_sockopt_level;
use super::common::{
    IPV6_RECVPKTINFO, SO_ERROR, SO_PEERCRED, SO_RCVBUF, SO_RCVTIMEO, SO_REUSEADDR, SO_SNDBUF,
    SO_SNDTIMEO,
};
use super::common::{SOL_IP, SOL_IPV6, SOL_SOCKET, SOL_TCP, TCP_CONGESTION, TCP_INFO, TCP_MAXSEG};

/// 查询 socket 选项值。
///
/// # Semantics
///
/// 按 `(level, optname)` 分发到对应读取逻辑。
/// 支持的选项：
/// - `SOL_SOCKET`：`SO_ERROR`（读并清除 pending error）、`SO_SNDBUF`/`SO_RCVBUF`、
///   `SO_REUSEADDR`、`SO_PEERCRED`（`pid,uid,gid` = 12 bytes）、
///   `SO_RCVTIMEO`/`SO_SNDTIMEO`（返回零值 `TimeVal`）。
/// - `SOL_TCP`：`TCP_MAXSEG`（返回 `TCP_MSS`）、`TCP_INFO`（返回 `TcpInfo` 结构体）、
///   `TCP_CONGESTION`（返回 `"reno"`）。
/// - `SOL_IPV6`：`IPV6_RECVPKTINFO`（返回 0）。
///
/// # Errors
///
/// - `-EFAULT`：`optval_ptr_ == 0` 或 `optlen == 0`。
/// - `-EINVAL`：`*optlen_val < 4`。
/// - `-ENOPROTOOPT`：已知 `level` 但未知 `optname`。
/// - `-EOPNOTSUPP`：`level` 与 socket 类型不兼容。
///
/// # Linux Compatibility
///
/// 错误码区分逻辑与 Linux 6.6 一致：已知 level + 未知 optname → `ENOPROTOOPT`；
/// 未知 level 或 level 不兼容 → `EOPNOTSUPP`。
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
    // optlen 校验必须在 optname/level 匹配之前（getsockopt01 期望 EINVAL 优先于 ENOPROTOOPT）
    let optlen_val = match optlen_ptr.read(token) {
        Ok(val) => val,
        Err(_) => return -(SyscallErr::EFAULT as isize),
    };
    if optlen_val < 4 {
        return -(SyscallErr::EINVAL as isize);
    }
    match (level, optname) {
        (SOL_SOCKET, SO_ERROR) => {
            // 读并清除 socket 待处理错误（非阻塞 connect 失败后 getsockopt(SO_ERROR)）
            let so_error = socket
                .take_error()
                .map(|e| (-(e as isize)) as u32)
                .unwrap_or(0);
            if optval_ptr.write(token, &so_error).is_err()
                || optlen_ptr.write(token, &4u32).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        (SOL_TCP, TCP_MAXSEG) => {
            // return max TCP segment size (MSS)
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
            // Safety: `&info` is a valid reference with lifetime lasting through the write.
            // `from_raw_parts` views the `TcpInfo` struct as bytes; `info_len` matches
            // `size_of::<TcpInfo>()`, so the slice is well-formed and write_from is safe.
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
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_REUSEADDR | SO_PEERCRED) => {
            // 对于需要写入 u32 的选项，检查 optlen 是否够大
            let optlen_val = match optlen_ptr.read(token) {
                Ok(len) => len,
                Err(_) => return -(SyscallErr::EFAULT as isize),
            };
            if (optlen_val as i32) < 0 || optlen_val < 4 {
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
                SO_PEERCRED => {
                    let (pid, uid, gid) = match socket.peer_creds() {
                        Ok(creds) => creds,
                        Err(e) => return -(e as isize),
                    };
                    // ucred: pid(4) + uid(4) + gid(4) = 12 bytes
                    let p_pid = UserPtrMut::<u32>::from_addr(optval_ptr_);
                    let p_uid = UserPtrMut::<u32>::from_addr(optval_ptr_ + 4);
                    let p_gid = UserPtrMut::<u32>::from_addr(optval_ptr_ + 8);
                    if p_pid.write(token, &pid).is_err()
                        || p_uid.write(token, &uid).is_err()
                        || p_gid.write(token, &gid).is_err()
                        || optlen_ptr.write(token, &12).is_err()
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
            // Safety: `&timeout` is a valid reference with lifetime through the write.
            // `len` equals `size_of::<TimeVal>()`, so the byte-slice view is well-formed.
            let bytes = unsafe {
                core::slice::from_raw_parts(&timeout as *const TimeVal as *const u8, len)
            };
            if writer.write_from(bytes).is_err() || optlen_ptr.write(token, &(len as u32)).is_err()
            {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        (SOL_IPV6, IPV6_RECVPKTINFO) => {
            let val: u32 = 0;
            if optval_ptr.write(token, &val).is_err() || optlen_ptr.write(token, &4u32).is_err() {
                return -(SyscallErr::EFAULT as isize);
            }
        }
        _ => {
            log::warn!("[sys_getsockopt] level: {}, optname: {}", level, optname);
            // 未知 level 或 level 与 socket 类型不兼容 → EOPNOTSUPP
            // 已知 level 但未知 optname → ENOPROTOOPT
            let s_type = socket.socket_type();
            let level_compat = level == SOL_SOCKET
                || level == SOL_IP
                || (level == SOL_TCP && matches!(s_type, PSOCK::Stream))
                || (level == 17 /* SOL_UDP */ && matches!(s_type, PSOCK::Datagram))
                || (level == 255 /* SOL_RAW */ && matches!(s_type, PSOCK::Raw));
            if level_compat {
                return -(SyscallErr::ENOPROTOOPT as isize);
            } else {
                return -(SyscallErr::EOPNOTSUPP as isize);
            }
        }
    }
    0 as isize
}
