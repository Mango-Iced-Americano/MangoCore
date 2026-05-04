use crate::net::{Endpoint, Socket, SocketFile, SocketType, SOCK_TYPE_MASK};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU16, Ordering};
use smoltcp::wire::IpListenEndpoint;

/// 全局端口管理器，对标 Linux 内核的临时端口分配。
/// 使用全局原子递增计数器，而非 RNG，避免 fork() 后父子进程端口碰撞。
/// 范围: 49152..=65534（Linux 默认临时端口范围）。
static NEXT_EPHEMERAL_PORT: AtomicU16 = AtomicU16::new(49152);
const EPHEMERAL_PORT_MIN: u16 = 49152;
const EPHEMERAL_PORT_MAX: u16 = 65534;

/// 全局端口管理器，对标 DragonOS `PortManager`。
/// 本项目单网卡，使用全局单例（静态方法集合）。
pub struct PortManager;

impl PortManager {
    /// 分配一个临时端口（ephemeral port），范围 49152..65534。
    /// 使用全局原子递增，绕过 fork() 克隆 RNG 状态导致端口碰撞的问题。
    pub fn alloc_ephemeral_port() -> u16 {
        let port = NEXT_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
        if port > EPHEMERAL_PORT_MAX {
            NEXT_EPHEMERAL_PORT.store(EPHEMERAL_PORT_MIN, Ordering::Relaxed);
            EPHEMERAL_PORT_MIN
        } else {
            port
        }
    }

    /// 检查 fd_table 中是否有其他 socket 与目标 endpoint 冲突（端口已占用）。
    /// 从 `crate::net::check_port_conflict` 移动而来。
    pub fn check_bind_conflict(
        task: &crate::task::TaskControlBlock,
        endpoint: IpListenEndpoint,
        target_sock: &Arc<dyn Socket>,
    ) -> bool {
        log::info!(
            "[PortManager::check_bind_conflict] check bind for endpoint {:?} with type {:?}",
            endpoint,
            target_sock.socket_type()
        );
        let target_pure_type = target_sock.socket_type().bits() & SOCK_TYPE_MASK;
        let fd_table = task.files.lock();
        for fd_opt in fd_table.iter() {
            let fd_ref = match fd_opt {
                Some(fd) => fd,
                None => continue,
            };
            let socket_file = match fd_ref.file.clone().downcast_arc::<SocketFile>() {
                Ok(sf) => sf,
                Err(_) => continue,
            };
            let socket = socket_file.inner.clone();
            let pure_type = socket.socket_type().bits() & SOCK_TYPE_MASK;
            if pure_type != target_pure_type {
                log::info!(
                    "[PortManager::check_bind_conflict] skip socket with different type: {:?}",
                    socket.socket_type()
                );
                continue;
            }
            let local = match socket.local_endpoint() {
                Some(Endpoint::Ip(ep)) => IpListenEndpoint {
                    addr: if ep.addr.is_unspecified() { None } else { Some(ep.addr) },
                    port: ep.port,
                },
                _ => continue, // 非 INET socket 不参与端口冲突检查
            };
            if local.port != endpoint.port || endpoint.port == 0 {
                continue;
            }

            let addr_confilct = match (local.addr, endpoint.addr) {
                (Some(local_addr), Some(endpoint_addr)) => local_addr == endpoint_addr,
                (None, _) | (_, None) => true,
            };
            if addr_confilct {
                if pure_type == SocketType::SOCK_DGRAM.bits() {
                    let reuse_enabled_on_exist = match socket.reuse_addr() {
                        Ok(_enabled) => true,
                        Err(_) => false,
                    };
                    let reuse_enabled_on_target = match target_sock.reuse_addr() {
                        Ok(_enabled) => true,
                        Err(_) => false,
                    };
                    if reuse_enabled_on_exist && reuse_enabled_on_target {
                        log::info!("[PortManager::check_bind_conflict] Bypass conflict because both sockets have SO_REUSEADDR enabled");
                        continue;
                    }
                    if socket.remote_endpoint().is_some() {
                        log::info!("[PortManager::check_bind_conflict] Bypass conflict because existing UDP socket is already connected to a remote");
                        continue;
                    }
                }
                log::info!(
                    "[PortManager::check_bind_conflict] Confilct local {:?} with endpoint {:?}",
                    local,
                    endpoint
                );
                return true;
            }
        }
        false
    }

    /// 绑定端口：先检查冲突，无冲突则调用 socket.bind()。
    /// `sys_bind` 应使用此方法替代手动 `check_port_conflict + socket.bind()`。
    pub fn bind_port(
        task: &crate::task::TaskControlBlock,
        socket: &Arc<dyn Socket>,
        endpoint: &Endpoint,
    ) -> crate::utils::error::SyscallRet {
        // 对于非 IP 端点（如 Unix），跳过端口冲突检查直接 bind
        let Endpoint::Ip(ep) = endpoint else {
            return socket.bind(endpoint);
        };
        // 转换为 IpListenEndpoint 进行冲突检查
        let listen_ep = if ep.addr.is_unspecified() {
            IpListenEndpoint {
                addr: None,
                port: ep.port,
            }
        } else {
            IpListenEndpoint {
                addr: Some(ep.addr),
                port: ep.port,
            }
        };
        if Self::check_bind_conflict(task, listen_ep, socket) {
            return Err(crate::utils::error::SyscallErr::EADDRINUSE);
        }
        socket.bind(endpoint)
    }
}
