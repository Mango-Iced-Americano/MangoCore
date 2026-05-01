pub fn sys_listen(sockfd: u32, _backlog: u32) -> isize {
    let socket = crate::get_socket!(sockfd);
    //socket.listen().unwrap() as isize
    match socket.listen() {
        Ok(s) => s as isize,
        Err(err) => -(err as isize),
    }
}
