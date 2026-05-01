use crate::net::address;
use crate::task::current_task;

use super::common::check_addrlen;

pub fn sys_bind(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    match check_addrlen(addrlen) {
        Ok(_) => {}
        Err(e) => return -(e as isize),
    }
    let addr_buf = crate::trans_ref!(addr, addrlen);
    let socket = crate::get_socket!(sockfd);
    let endpoint = match address::listen_endpoint(addr_buf) {
        Ok(ep) => ep,
        Err(e) => return -(e as isize),
    };
    let task = current_task().unwrap();
    match crate::net::socket::inet::common::PortManager::bind_port(&task, &socket, endpoint) {
        Ok(_) => 0 as isize,
        Err(e) => -(e as isize),
    }
}
