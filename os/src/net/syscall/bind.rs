use super::common::check_addrlen;
use crate::fs::directory_tree::DirectoryTreeNode;
use crate::fs::DiskInodeType;
use crate::get_socket;
use crate::net::socket::unix::ns::{ABSTRACT_TABLE, UNIX_PATH_MAX};
use crate::net::socket::unix::PATH_TABLE;
use crate::net::socket::UnixEndpoint;
use crate::net::Endpoint;
use crate::task::current_task;
use crate::utils::error::SyscallErr;
use alloc::format;
use alloc::string::ToString;
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

                    // parent_node 是 FileDescriptor，用于底层 create
                    let parent_node = match cwd_node.cd(parent_path) {
                        Ok(node) => node,
                        Err(_) => return -(SyscallErr::ENOENT as isize),
                    };
                    // parent_dir_node 是 DirectoryTreeNode，用于 VFS 缓存操作
                    let parent_dir_node = match parent_node.file.get_dirtree_node() {
                        Some(node) => node,
                        None => return -(SyscallErr::ENOENT as isize),
                    };

                    // 通过 VFS 缓存检查文件是否已存在（同步磁盘 + 内存）
                    let mut vfs_lock = parent_dir_node.children.write();
                    if parent_dir_node
                        .try_to_open_subfile(file_name, &mut vfs_lock)
                        .is_ok()
                    {
                        return -(SyscallErr::EADDRINUSE as isize);
                    }
                    // create 成功后 vfs_lock 仍被持有，后续插入缓存

                    // 在磁盘上创建 socket 文件
                    let new_file = match parent_node.file.create(file_name, DiskInodeType::Socket) {
                        Ok(file) => file,
                        Err(e) if e == -(SyscallErr::EEXIST as isize) => {
                            return -(SyscallErr::EADDRINUSE as isize);
                        }
                        Err(_) => return -(SyscallErr::EACCES as isize),
                    };

                    // 将新文件插入 VFS 缓存（参照 DirectoryTreeNode::open 模式）
                    let key = file_name.to_string();
                    let vfs_node = DirectoryTreeNode::new(
                        key.clone(),
                        parent_dir_node.filesystem.clone(),
                        new_file,
                        Arc::downgrade(&parent_dir_node),
                    );
                    vfs_lock.as_mut().unwrap().insert(key.clone(), vfs_node);
                    drop(vfs_lock);

                    let parent_abs = parent_dir_node.get_cwd();

                    let absolute_path = if parent_abs == "/" {
                        format!("/{}", file_name)
                    } else {
                        format!("{}/{}", parent_abs, file_name)
                    };

                    let socket = get_socket!(sockfd);
                    PATH_TABLE
                        .lock()
                        .insert(absolute_path.clone(), Arc::downgrade(&socket));

                    let full_endpoint = Endpoint::Unix(UnixEndpoint::Path(absolute_path.clone()));
                    match socket.bind(&full_endpoint) {
                        Ok(_) => 0 as isize,
                        Err(e) => {
                            // 回滚：从 PATH_TABLE 和 VFS 缓存中移除
                            // 磁盘文件保留（bind 对 Path 不会失败，此回滚仅防御性编程）
                            PATH_TABLE.lock().remove(&absolute_path);
                            let mut vfs_lock = parent_dir_node.children.write();
                            if let Some(map) = vfs_lock.as_mut() {
                                map.remove(&key);
                            }
                            -(e as isize)
                        }
                    }
                }
            }
        }
        Endpoint::Unspecified => -(SyscallErr::EINVAL as isize),
    }
}
