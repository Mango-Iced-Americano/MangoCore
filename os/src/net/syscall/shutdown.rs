use crate::utils::error::SyscallErr;

pub fn sys_sock_shutdown(sockfd: u32, how: u32) -> isize {
    log::info!("[sys_shutdown] sockfd {}, how {}", sockfd, how);
    let socket = crate::get_socket!(sockfd);
    let ret = socket.shutdown(how);
    match ret {
        Ok(_) => 0 as isize,
        Err(errno) => -(errno as isize),
    }
}
