use super::common::check_addrlen;
use crate::fs::DiskInodeType;
use crate::get_socket;
use crate::net::socket::unix::ns::{ABSTRACT_TABLE, UNIX_PATH_MAX};
use crate::net::socket::unix::PATH_TABLE;
use crate::net::socket::UnixEndpoint;
use crate::net::Endpoint;
use crate::task::current_task;
use crate::utils::error::SyscallErr;
use alloc::format;
use alloc::sync::Arc;

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
                UnixEndpoint::Path(ref path) => {
                    let task = current_task().unwrap();
                    let cwd_node = task.fs.lock().working_inode.clone();

                    let (parent_path, file_name) = match path.rfind('/') {
                        Some(idx) => {
                            if idx == 0 {
                                ("/", &path[1..])
                            } else {
                                (&path[..idx], &path[idx + 1..])
                            }
                        }
                        None => (".", path.as_str()),
                    };

                    let parent_node = match cwd_node.cd(parent_path) {
                        Ok(node) => node,
                        Err(_) => return -(SyscallErr::ENOENT as isize),
                    };

                    match parent_node.file.create(file_name, DiskInodeType::Socket) {
                        Ok(_) => {}
                        Err(e) if e == -(SyscallErr::EEXIST as isize) => {
                            return -(SyscallErr::EADDRINUSE as isize);
                        }
                        Err(_) => return -(SyscallErr::EACCES as isize),
                    };

                    let parent_abs = match parent_node.get_cwd() {
                        Some(path) => path,
                        None => return -(SyscallErr::ENOENT as isize),
                    };

                    let absolute_path = if parent_abs == "/" {
                        format!("/{}", file_name)
                    } else {
                        format!("{}/{}", parent_abs, file_name)
                    };

                    let socket = get_socket!(sockfd);
                    PATH_TABLE
                        .lock()
                        .insert(absolute_path.clone(), Arc::downgrade(&socket));

                    let full_endpoint = Endpoint::Unix(UnixEndpoint::Path(absolute_path));
                    match socket.bind(&full_endpoint) {
                        Ok(_) => 0 as isize,
                        Err(e) => -(e as isize),
                    }
                }
            }
        }
        Endpoint::Unspecified => -(SyscallErr::EINVAL as isize),
    }
}
