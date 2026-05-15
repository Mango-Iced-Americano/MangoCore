use alloc::sync::Arc;
use core::convert::TryFrom;
use log::info;

use crate::fs::FileDescriptor;
use crate::mm::UserSlice;
use crate::net::posix::PosixArgsSocketType;
use crate::net::{make_unix_socket_pair, SocketFile, AF_UNIX, AF_UNSPEC, PSOCK};
use crate::task::current_task;
use crate::utils::error::SyscallErr;

pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize {
    info!(
        "[sys_socketpair] domain {}, type {}, protocol {}, sv {}",
        domain, socket_type, protocol, sv
    );

    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
    let is_nonblock = type_arg.is_nonblock();
    let is_cloexec = type_arg.is_cloexec();

    // Linux socketpair() only supports AF_UNIX/AF_LOCAL
    match domain as u16 {
        AF_UNIX | AF_UNSPEC => {
            // AF_UNIX 和 AF_UNSPEC 都走 unix socket pair
        }
        _ => {
            info!(
                "[sys_socketpair] domain {} not supported, only AF_UNIX is allowed",
                domain
            );
            return -(SyscallErr::EPROTONOSUPPORT as isize);
        }
    }

    // 仅支持 SOCK_STREAM 和 SOCK_DGRAM 的 socketpair
    let (socket1, socket2): (Arc<dyn crate::net::Socket>, Arc<dyn crate::net::Socket>) = match psock
    {
        PSOCK::Stream | PSOCK::Datagram => {
            let (s1, s2) = make_unix_socket_pair(is_nonblock, psock);
            (s1, s2)
        }
        _ => {
            return -(SyscallErr::ESOCKTNOSUPPORT as isize);
        }
    };

    let socket_file1 = Arc::new(SocketFile::new(socket1));
    let socket_file2 = Arc::new(SocketFile::new(socket2));

    let task = current_task().unwrap();
    let fd1 = task
        .files
        .lock()
        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file1))
        .unwrap();
    let fd2 = task
        .files
        .lock()
        .insert(FileDescriptor::new(is_cloexec, is_nonblock, socket_file2))
        .unwrap();

    // 将两个 fd 写入用户空间的 sv 数组（sv[0] = fd1, sv[1] = fd2）
    let token = task.get_user_token();
    let fds = [fd1 as u32, fd2 as u32];
    if UserSlice::new(sv as *const u32, 2)
        .write_array_from(token, &fds)
        .is_err()
    {
        return -(SyscallErr::EFAULT as isize);
    }

    info!("[sys_socketpair] new sv: [{}, {}]", fd1, fd2);
    0
}
