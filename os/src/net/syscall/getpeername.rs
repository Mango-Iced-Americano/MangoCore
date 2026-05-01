pub fn sys_getpeername(sockfd: u32, addr: usize, addrlen: usize) -> isize {
    let socket = crate::get_socket!(sockfd);
    // socket.peer_addr(addr, addrlen).unwrap() as isize
    match socket.peer_addr(addr, addrlen) {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}
