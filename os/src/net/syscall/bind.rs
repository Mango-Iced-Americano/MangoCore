use super::common::check_addrlen;
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
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address, Ipv6Address};

const CAP_NET_BIND_SERVICE: usize = 10;

fn is_local_bind_addr(addr: IpAddress) -> bool {
    if addr.is_unspecified() {
        return true;
    }
    match addr {
        IpAddress::Ipv4(ip) => {
            ip.is_loopback() || crate::net::net_core::is_local_addr(ip)
        }
        IpAddress::Ipv6(ip) => {
            if ip == Ipv6Address::LOOPBACK {
                return true;
            }
            if let Some(v4) = ip.as_ipv4() {
                return v4.is_loopback() || crate::net::net_core::is_local_addr(v4);
            }
            false
        }
    }
}

fn ipv4_endpoint_from_unspec_sockaddr(addr_buf: &[u8]) -> Option<Endpoint> {
    if addr_buf.len() < 16 {
        return None;
    }
    let port = u16::from_be_bytes([addr_buf[2], addr_buf[3]]);
    let ip = Ipv4Address::from_bytes(&[addr_buf[4], addr_buf[5], addr_buf[6], addr_buf[7]]);
    Some(Endpoint::Ip(IpEndpoint::new(IpAddress::Ipv4(ip), port)))
}

/// 将 socket 绑定到本地地址。
///
/// # Semantics
///
/// 校验 `addrlen` 后解析 `sockaddr` → `Endpoint`，按地址族分发：
/// - `Ip`：检查 `is_local_bind_addr`（仅允许本地/环回/通配地址），特权端口检查
///   （`CAP_NET_BIND_SERVICE`），调用 `PortManager::bind_port` 注册。
/// - `Unix`：创建 socket 文件（`Path`）或注册抽象命名空间（`Abstract`），
///   失败时回滚 `PATH_TABLE` 条目。
/// - `Netlink`/`Packet`/`Unspecified`：直接委托 `socket.bind()`。
///
/// # Errors
///
/// - `-EINVAL`：`addrlen` 超限（`>512`）或抽象名称为空/过长。
/// - `-EFAULT`：用户态 `addr` 非法。
/// - `-EADDRNOTAVAIL`：尝试绑定非本地 IP 地址。
/// - `-EACCES`：非 root 进程绑定特权端口（`<1024`）且无 `CAP_NET_BIND_SERVICE`。
/// - `-EADDRINUSE`：Unix path socket 文件已存在。
/// - `-EAFNOSUPPORT`：IP socket 绑定 Unix 端点（通过 `local_endpoint` 预检）。
///
/// # Linux Compatibility
///
/// 特权端口检查使用 `CAP_NET_BIND_SERVICE` 而非传统的 `uid==0` 检查 ——
/// 两者的行为等价于 Linux 6.6。
pub fn sys_bind(sockfd: u32, addr: usize, addrlen: u32) -> isize {
    match check_addrlen(addrlen) {
        Ok(_) => {}
        Err(e) => return -(e as isize),
    }
    let addr_buf = crate::trans_ref!(addr, addrlen);
    let endpoint = match Endpoint::from_sockaddr(addr_buf) {
        Ok(Endpoint::Unspecified) => {
            ipv4_endpoint_from_unspec_sockaddr(addr_buf).unwrap_or(Endpoint::Unspecified)
        }
        Ok(ep) => ep,
        Err(e) => return -(e as isize),
    };
    match endpoint {
        Endpoint::Ip(ep) => {
            if !is_local_bind_addr(ep.addr) {
                return -(SyscallErr::EADDRNOTAVAIL as isize);
            }
            let endpoint = Endpoint::Ip(ep);
            let socket = crate::get_socket!(sockfd);
            let task = current_task().unwrap();
            if endpoint.port() < 1024 {
                let inner = task.acquire_inner_lock();
                if inner.euid != 0
                    && (inner.cap_effective & (1u64 << CAP_NET_BIND_SERVICE)) == 0
                {
                    return -(SyscallErr::EACCES as isize);
                }
            }
            match crate::net::socket::inet::common::PortManager::bind_port(
                &task, &socket, &endpoint,
            ) {
                Ok(_) => 0 as isize,
                Err(e) => -(e as isize),
            }
        }
        Endpoint::Unix(ep) => {
            let socket = crate::get_socket!(sockfd);

            // Domain 兼容性检查：AF_INET/AF_INET6 socket 绑定 Unix 路径应返回 EAFNOSUPPORT。
            // 检查 socket 是否能处理 Unix 端点：IP socket 在 bind(Unix) 上返回 EINVAL，
            // 但 Linux 语义要求非 AF_UNIX socket 绑定 Unix 地址时返回 EAFNOSUPPORT。
            // 这里通过预检查快速检测：如果 socket 的 socket_type() 能区分，但 Unix 和 IP
            // 的 Stream/Datagram 共用相同的 PSOCK 值，因此直接查询类型不够。
            // 安全做法：先轻量检查 local_endpoint 的模式（仅已绑定 socket 有值），
            // 再通过尝试 bind 来检测兼容性。
            let is_compat = match socket.local_endpoint() {
                // 如果已有绑定的 endpoint，检查其 domain 是否匹配 Unix
                Some(Endpoint::Unix(_)) | None => {
                    // 未绑定或已绑定 Unix → 可能兼容
                    true
                }
                Some(_) => {
                    // 已绑定 IP endpoint → 不兼容 Unix
                    false
                }
            };
            if !is_compat {
                return -(SyscallErr::EAFNOSUPPORT as isize);
            }

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
                    let cwd_node = task.process.fs().lock().working_inode.clone();

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

                    let start = cwd_node.inode.clone();
                    let parent_node = match crate::fs::vfs_lookup(&start, parent_path, true) {
                        Ok(node) => node,
                        Err(errno) => return errno,
                    };
                    match parent_node.metadata() {
                        Ok(meta) if meta.file_type != crate::fs::vfs::FileType::Dir => {
                            return -(SyscallErr::ENOTDIR as isize);
                        }
                        Ok(_) => {}
                        Err(e) => return -(e as isize),
                    }

                    // 检查文件是否已存在
                    if parent_node.find(file_name).is_ok() {
                        return -(SyscallErr::EADDRINUSE as isize);
                    }

                    // 在磁盘上创建 socket 文件（新 VFS API）
                    let _new_inode = match parent_node.create(
                        file_name,
                        crate::fs::vfs::FileType::Socket,
                        crate::fs::vfs::InodeMode::S_IRWXUGO,
                    ) {
                        Ok(inode) => inode,
                        Err(e) if e == SyscallErr::EEXIST => {
                            return -(SyscallErr::EADDRINUSE as isize);
                        }
                        Err(e) => return -(e as isize),
                    };

                    // 生成绝对路径
                    let parent_abs = parent_node.absolute_path().unwrap_or_default();
                    let absolute_path = if parent_abs == "/" || parent_abs.is_empty() {
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
                            // 回滚：从 PATH_TABLE 中移除
                            PATH_TABLE.lock().remove(&absolute_path);
                            -(e as isize)
                        }
                    }
                }
            }
        }
        Endpoint::Netlink(nl_pid) => {
            let socket = crate::get_socket!(sockfd);
            match socket.bind(&Endpoint::Netlink(nl_pid)) {
                Ok(_) => 0 as isize,
                Err(e) => -(e as isize),
            }
        }
        Endpoint::Packet(ep) => {
            let socket = crate::get_socket!(sockfd);
            match socket.bind(&Endpoint::Packet(ep)) {
                Ok(_) => 0 as isize,
                Err(e) => -(e as isize),
            }
        }
        Endpoint::Unspecified => {
            let socket = crate::get_socket!(sockfd);
            match socket.bind(&Endpoint::Unspecified) {
                Ok(_) => 0 as isize,
                Err(e) => -(e as isize),
            }
        }
    }
}
