use alloc::sync::Arc;
use core::convert::TryFrom;
use log::info;

use crate::config::PAGE_SIZE;
use crate::fs::FileDescriptor;
use crate::net::posix::PosixArgsSocketType;
use crate::net::{make_unix_socket_pair, PSOCK, SocketFile, AF_UNIX, AF_UNSPEC};
use crate::task::current_task;
use crate::utils::error::SyscallErr;

pub fn sys_socketpair(domain: u32, socket_type: u32, protocol: u32, sv: usize) -> isize {
    info!(
        "[sys_socketpair] domain {}, type {}, protocol {}, sv {}",
        domain, socket_type, protocol, sv
    );

    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let _psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
    let _is_nonblock = type_arg.is_nonblock();
    let _is_cloexec = type_arg.is_cloexec();

    // Linux socketpair() only supports AF_UNIX/AF_LOCAL
    match domain as u16 {
        AF_UNIX => {
            // 当前实现：仅支持 AF_UNIX，直接走已有的 unix socket pair 实现
            return SyscallErr::EAFNOSUPPORT as isize;
        }
        AF_UNSPEC => {
            // glibc 的一些内部接口可能传 AF_UNSPEC，暂时同等对待 AF_UNIX
            // TODO: 严格模式下应返回 EAFNOSUPPORT
            info!("[sys_socketpair] domain=AF_UNSPEC, treating as AF_UNIX");
            return SyscallErr::EAFNOSUPPORT as isize;
        }
        _ => {
            info!("[sys_socketpair] domain {} not supported, only AF_UNIX is allowed", domain);
            return SyscallErr::EAFNOSUPPORT as isize;
        }
    }

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
