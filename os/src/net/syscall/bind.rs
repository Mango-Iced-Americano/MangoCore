use super::common::check_addrlen;
use crate::net::socket::unix::ns::{ABSTRACT_TABLE, UNIX_PATH_MAX};
use crate::net::socket::UnixEndpoint;
use crate::net::Endpoint;
use crate::task::current_task;
use crate::utils::error::{SyscallErr, SyscallRet};

pub fn sys_bind(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    match check_addrlen(addrlen) {
        Ok(_) => {}
        Err(e) => return -(e as isize),
    }
    let addr_buf = crate::trans_ref!(addr, addrlen);
    let endpoint = match Endpoint::from_sockaddr(addr_buf) {
        Ok(ep) => ep,
        Err(e) => return -(e as isize),
    };
    match endpoint {
        Endpoint::Ip(_) => {
            let socket = crate::get_socket!(sockfd);
            let task = current_task().unwrap();
            match crate::net::socket::inet::common::PortManager::bind_port(
                &task, &socket, &endpoint,
            ) {
                Ok(_) => 0 as isize,
                Err(e) => -(e as isize),
            }
        }
        Endpoint::Unix(ep) => {
            let socket = crate::get_socket!(sockfd);
            let task = current_task().unwrap();
            match ep {
                UnixEndpoint::Unnamed => {
                    match socket.bind(&Endpoint::Unix(UnixEndpoint::Unnamed)) {
                        Ok(_) => 0 as isize,
                        Err(e) => -(e as isize),
                    }
                }
                UnixEndpoint::Abstract(name) => {
                    if name.is_empty() || name.len() > UNIX_PATH_MAX - 1 {
                        return -(SyscallErr::EINVAL as isize);
                    }

                    match socket.bind(&Endpoint::Unix(UnixEndpoint::Abstract(name.clone()))) {
                        Ok(_) => {}
                        Err(e) => return -(e as isize),
                    }

                    ABSTRACT_TABLE
                        .create_abstract_name_bytes(&name, socket.clone())
                        .map(|_| 0)
                        .unwrap_or_else(|e| -(e as isize))
                }

                _ => -(SyscallErr::EINVAL as isize),
            }
        }
        Endpoint::Unspecified => -(SyscallErr::EINVAL as isize),
    }
}
