use core::convert::TryFrom;
use log::info;

use crate::net::posix::PosixArgsSocketType;
use crate::net::{PSOCK, Socket};

pub fn sys_socket(domain: u32, socket_type: u32, protocol: u32) -> isize {
    info!(
        "[sys_socket] domain: {}, type: {}, protocol: {}",
        domain, socket_type, protocol
    );
    // 在 syscall 入口处解析 raw u32 → PSOCK + bool flags
    let type_arg = PosixArgsSocketType::from_bits_truncate(socket_type);
    let psock = match PSOCK::try_from(type_arg) {
        Ok(s) => s,
        Err(e) => return -(e as isize),
    };
    let is_nonblock = type_arg.is_nonblock();
    let is_cloexec = type_arg.is_cloexec();
    let result = match crate::net::Socket::alloc(domain, psock, protocol, is_nonblock, is_cloexec) {
        Ok(sockfd) => {
            info!("[sys_socket] new sockfd: {}", sockfd);
            sockfd as isize
        }
        Err(e) => {
            info!("[sys_socket] new sockfd failed");
            -(e as isize)
        }
    };
    result
}
