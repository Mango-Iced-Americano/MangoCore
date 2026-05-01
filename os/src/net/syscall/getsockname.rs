pub fn sys_getsockname(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    // socket.addr(addr, addrlen).unwrap() as isize
    match socket.addr(addr, addrlen) {
        Ok(new_fd) => new_fd as isize,
        Err(err) => -(err as isize),
    }
}
