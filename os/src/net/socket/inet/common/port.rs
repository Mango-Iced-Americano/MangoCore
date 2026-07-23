use crate::net::net_core;
use crate::net::{Endpoint, Socket, SocketFile, PSOCK};
use crate::utils::error::SyscallErr;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};
use lazy_static::*;
use smoltcp::wire::{IpAddress, IpListenEndpoint, Ipv4Address};
use spin::Mutex;

/// 全局端口管理器，对标 Linux 内核的临时端口分配。
/// 使用全局原子递增计数器，而非 RNG，避免 fork() 后父子进程端口碰撞。
/// 范围: 49152..=65534（Linux 默认临时端口范围）。
static NEXT_EPHEMERAL_PORT: AtomicU16 = AtomicU16::new(49152);
const EPHEMERAL_PORT_MIN: u16 = 49152;
const EPHEMERAL_PORT_MAX: u16 = 65534;

#[derive(Clone)]
pub struct PortBinding {
    pub port: u16,
    pub addr: Option<Ipv4Address>,
    pub socket_weak: Weak<dyn Socket>,
}

#[derive(Clone)]
pub struct UdpPortBinding {
    pub port: u16,
    pub addr: Option<Ipv4Address>,
    pub reuseaddr: bool,
    pub reuseport: bool,
    pub socket_weak: Weak<dyn Socket>,
}

lazy_static! {
    /// TCP 端口绑定表：port -> PortBinding
    pub static ref TCP_PORTS: Mutex<BTreeMap<u16, PortBinding>> = Mutex::new(BTreeMap::new());
    /// UDP 端口绑定表：port -> Vec<UdpPortBinding>
    pub static ref UDP_PORTS: Mutex<BTreeMap<u16, Vec<UdpPortBinding>>> = Mutex::new(BTreeMap::new());
}

/// 全局端口管理器，对标 DragonOS `PortManager`。
/// 本项目单网卡，使用全局单例（静态方法集合）。
pub struct PortManager;

impl PortManager {
    /// 分配一个临时端口（ephemeral port）。
    /// 使用全局原子递增起始偏移，结合 `net_core::local_port_range()` 动态范围，
    /// 并跳过 TCP_PORTS / UDP_PORTS 中已绑定的端口。
    pub fn alloc_ephemeral_port() -> u16 {
        let (min, max) = net_core::local_port_range();
        let start = NEXT_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
        let mut port = if start < min || start > max {
            NEXT_EPHEMERAL_PORT.store(min, Ordering::Relaxed);
            min
        } else {
            start
        };
        let orig = port;
        loop {
            let tcp_taken = TCP_PORTS.lock().contains_key(&port);
            let udp_taken = UDP_PORTS.lock().contains_key(&port);
            if !tcp_taken && !udp_taken {
                return port;
            }
            port = if port >= max { min } else { port + 1 };
            if port == orig {
                break;
            }
        }
        0
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
        let target_pure_type = target_sock.socket_type();
        let port = endpoint.port;
        let addr = Self::addr_to_ipv4(endpoint.addr);

        // Priority 1: Check port tables first (faster, covers cross-process bindings)
        // TIME_WAIT handling: TCP ports are unregistered from TCP_PORTS when the socket
        // is fully closed and removed from SocketSet (via UDP_SOCKETS_TO_REMOVE /
        // TCP_SOCKETS_TO_REMOVE mechanism in config.rs). Until then, the port remains occupied.
        match target_pure_type {
            PSOCK::Stream => {
                if Self::check_tcp_conflict(port, addr) {
                    log::info!(
                        "[PortManager::check_bind_conflict] TCP port {} occupied according to TCP_PORTS table",
                        port
                    );
                    return true;
                }
            }
            PSOCK::Datagram => {
                let reuseaddr = target_sock.reuse_addr().is_ok();
                if Self::check_udp_conflict(port, addr, reuseaddr).is_err() {
                    log::info!(
                        "[PortManager::check_bind_conflict] UDP port {} occupied according to UDP_PORTS table",
                        port
                    );
                    return true;
                }
            }
            _ => {}
        }

        // Priority 2: Fallback to fd_table scan (for sockets not yet tracked in port tables)
        let files_ref = task.process.files();
        let fd_table = files_ref.lock();
        for (_fd_num, file) in fd_table.iter() {
            let socket_file = match file.inode.as_any_ref().downcast_ref::<SocketFile>() {
                Some(sf) => sf,
                None => continue,
            };
            let socket = socket_file.inner.clone();
            let pure_type = socket.socket_type();
            if pure_type != target_pure_type {
                log::info!(
                    "[PortManager::check_bind_conflict] skip socket with different type: {:?}",
                    socket.socket_type()
                );
                continue;
            }
            let local = match socket.local_endpoint() {
                Some(Endpoint::Ip(ep)) => IpListenEndpoint {
                    addr: if ep.addr.is_unspecified() {
                        None
                    } else {
                        Some(ep.addr)
                    },
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
                if pure_type == PSOCK::Datagram {
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
            log::debug!(
                "bind conflict: port={} type={:?}",
                ep.port,
                socket.socket_type()
            );
            return Err(crate::utils::error::SyscallErr::EADDRINUSE);
        }
        let ret = socket.bind(endpoint);
        if ret.is_ok() {
            let actual_port = socket
                .local_endpoint()
                .map(|lep| match lep {
                    Endpoint::Ip(ip_ep) => ip_ep.port,
                    _ => ep.port,
                })
                .unwrap_or(ep.port);
            let ifindex = crate::net::net_core::ifindex_for_local_addr(listen_ep.addr);
            match socket.socket_type() {
                PSOCK::Stream => {
                    log::debug!(
                        "bind: {:?}:{} ifindex={} type={:?}",
                        listen_ep.addr,
                        actual_port,
                        ifindex,
                        PSOCK::Stream,
                    );
                    log::info!("[PortManager] bind success: port={} type=TCP", actual_port);
                    Self::register_tcp_bind(
                        actual_port,
                        Self::addr_to_ipv4(listen_ep.addr),
                        socket,
                    );
                }
                PSOCK::Datagram => {
                    let reuseaddr = socket.reuse_addr().is_ok();
                    log::debug!(
                        "bind: {:?}:{} ifindex={} type={:?}",
                        listen_ep.addr,
                        actual_port,
                        ifindex,
                        PSOCK::Datagram,
                    );
                    log::info!("[PortManager] bind success: port={} type=UDP", actual_port);
                    Self::register_udp_bind(
                        actual_port,
                        Self::addr_to_ipv4(listen_ep.addr),
                        reuseaddr,
                        reuseaddr,
                        socket,
                    );
                }
                _ => {}
            }
        }
        ret
    }

    /// 注册 TCP 端口绑定到全局 TCP_PORTS 表。
    pub fn register_tcp_bind(port: u16, addr: Option<Ipv4Address>, socket: &Arc<dyn Socket>) {
        let mut table = TCP_PORTS.lock();
        // 清理已失效的 Weak 引用
        table.retain(|_, v| v.socket_weak.upgrade().is_some());
        table.insert(
            port,
            PortBinding {
                port,
                addr,
                socket_weak: Arc::downgrade(socket),
            },
        );
    }

    /// 从全局 TCP_PORTS 表注销端口绑定。
    pub fn unregister_tcp_bind(port: u16) {
        TCP_PORTS.lock().remove(&port);
    }

    /// 注册 UDP 端口绑定到全局 UDP_PORTS 表。
    pub fn register_udp_bind(
        port: u16,
        addr: Option<Ipv4Address>,
        reuseaddr: bool,
        reuseport: bool,
        socket: &Arc<dyn Socket>,
    ) {
        let mut table = UDP_PORTS.lock();
        // 清理所有已失效的 Weak 引用
        for list in table.values_mut() {
            list.retain(|b| b.socket_weak.upgrade().is_some());
        }
        table.entry(port).or_default().push(UdpPortBinding {
            port,
            addr,
            reuseaddr,
            reuseport,
            socket_weak: Arc::downgrade(socket),
        });
    }

    /// 从全局 UDP_PORTS 表注销端口绑定。
    pub fn unregister_udp_bind(port: u16) {
        UDP_PORTS.lock().remove(&port);
    }

    /// 检查 TCP 端口冲突（true = 已占用）。
    pub fn check_tcp_conflict(port: u16, addr: Option<Ipv4Address>) -> bool {
        let mut table = TCP_PORTS.lock();
        // 清理已失效的 Weak 引用（socket 已 drop 但表项未及时清除）
        table.retain(|_, v| v.socket_weak.upgrade().is_some());
        if let Some(binding) = table.get(&port) {
            match (binding.addr, addr) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
        } else {
            false
        }
    }

    /// 检查 UDP 端口冲突。
    /// 返回 Ok(()) 表示无冲突或双方都启用了 SO_REUSEADDR。
    /// 返回 Err(EADDRINUSE) 表示端口已被占用。
    pub fn check_udp_conflict(
        port: u16,
        addr: Option<Ipv4Address>,
        reuseaddr: bool,
    ) -> Result<(), SyscallErr> {
        let mut table = UDP_PORTS.lock();
        // 清理已失效的 Weak 引用，同时移除空列表
        for list in table.values_mut() {
            list.retain(|b| b.socket_weak.upgrade().is_some());
        }
        table.retain(|_, v| !v.is_empty());
        if let Some(bindings) = table.get(&port) {
            for binding in bindings {
                let addr_conflict = match (binding.addr, addr) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                };
                if addr_conflict && !(reuseaddr && binding.reuseaddr) {
                    return Err(SyscallErr::EADDRINUSE);
                }
            }
        }
        Ok(())
    }

    /// 将 Option<IpAddress> 转换为 Option<Ipv4Address>，忽略 IPv6 地址。
    fn addr_to_ipv4(addr: Option<IpAddress>) -> Option<Ipv4Address> {
        addr.and_then(|a| match a {
            IpAddress::Ipv4(ip) => Some(ip),
            _ => None,
        })
    }
}
