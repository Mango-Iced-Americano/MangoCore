//! TCP 生命周期操作 —— bind / connect / listen / accept / shutdown

use crate::net::{
    config::{lookup_source_ip, route_check, NET_INTERFACE},
    routing::InetProtocol,
    socket::inet::common::PortManager,
    TCP_SOCKETS_TO_REMOVE,
};
use crate::trace_event;
use crate::utils::error::{GeneralRet, SyscallErr};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use smoltcp::{
    socket::tcp::{self, SocketBuffer},
    time::Duration,
    wire::{IpAddress, IpEndpoint, IpListenEndpoint, IpVersion},
};

use super::inner::{
    tcp_state_code, with_tcp_mut, Connecting, Established, Init, Inner, Listening, SelfConnected,
    DEFAULT_RX_BUF_SIZE, DEFAULT_TX_BUF_SIZE, LISTEN_BUFFER_SIZE,
};

impl Inner {
    /// bind 操作：将 Unbound 变为 Bound
    pub fn bind(self, endpoint: IpListenEndpoint) -> Result<Self, (Self, SyscallErr)> {
        match self {
            Inner::Init(Init::Unbound(socket, ver)) => {
                let port = if endpoint.port == 0 {
                    PortManager::alloc_ephemeral_port()
                } else {
                    endpoint.port
                };

                // 构造本地 endpoint
                let local_addr = endpoint.addr.unwrap_or_else(|| match ver {
                    IpVersion::Ipv4 => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
                    IpVersion::Ipv6 => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED),
                });
                let local = IpEndpoint::new(local_addr, port);

                Ok(Inner::Init(Init::Bound { socket, local, pending_error: None }))
            }
            Inner::Init(Init::Bound { .. }) => {
                log::debug!("[TCP] already bound");
                Err((self, SyscallErr::EINVAL))
            }
            other => Err((other, SyscallErr::EINVAL)),
        }
    }

    /// connect 操作：Unbound 先 bind_ephemeral → 发起 SYN
    /// If `bound_ifindex` is Some, use it instead of route-based ifindex lookup.
    pub fn connect(self, remote_endpoint: IpEndpoint, bound_ifindex: Option<u32>) -> Result<Connecting, (Self, SyscallErr)> {
        let (socket, local, ver) = match self {
            Inner::Init(Init::Unbound(_, ver)) => {
                let port = PortManager::alloc_ephemeral_port();
                let local_addr = lookup_source_ip(remote_endpoint.addr);
                let local = IpEndpoint::new(local_addr, port);
                let socket = {
                    let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                    let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                    tcp::Socket::new(rx_buf, tx_buf)
                };
                (socket, local, ver)
            }
            Inner::Init(Init::Bound { socket, local, pending_error: None }) => {
                let local = if local.addr.is_unspecified() {
                    IpEndpoint::new(lookup_source_ip(remote_endpoint.addr), local.port)
                } else {
                    local
                };
                let ver = match local.addr {
                    IpAddress::Ipv4(_) => IpVersion::Ipv4,
                    _ => IpVersion::Ipv6,
                };
                (*socket, local, ver)
            }
            other => return Err((other, SyscallErr::EISCONN)),
        };

        if let Err(e) = route_check(remote_endpoint.addr) {
            log::info!("[tcp::connect] route_check failed for {:?}: {:?}", remote_endpoint.addr, e);
            let new_sock = Box::new(socket);
            return Err((Inner::Init(Init::Bound { socket: new_sock, local, pending_error: Some(e) }), e));
        }

        // Route to determine target ifindex for this connection
        // SO_BINDTODEVICE overrides route-based ifindex
        let ifindex = bound_ifindex.unwrap_or_else(|| {
            let route = crate::net::routing::route_output(remote_endpoint.addr).ok();
            route.as_ref().map(|r| r.ifindex)
                .unwrap_or_else(|| crate::net::net_core::ifindex_for_local_addr(Some(local.addr)))
        });

        let handle = NET_INTERFACE
            .add_routed_socket_on(InetProtocol::Tcp, socket, ifindex)
            .ok_or_else(|| {
                let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                let new_sock = Box::new(tcp::Socket::new(rx_buf, tx_buf));
                (Inner::Init(Init::Bound { socket: new_sock, local, pending_error: None }), SyscallErr::EAGAIN)
            })?;

        let ret = NET_INTERFACE
            .tcp_connect(handle, remote_endpoint, local)
            .ok_or(SyscallErr::ECONNREFUSED)
            .and_then(|r| r.map_err(|_| SyscallErr::ECONNREFUSED));

        match ret {
            Ok(()) => {
                log::info!(
                    "[TCP::connect] initiated: local {:?} -> remote {:?}",
                    local,
                    remote_endpoint
                );
                trace_event!(
                    0xB021,
                    handle.0 as u64,
                    local.port as u64,
                    remote_endpoint.port as u64,
                    0,
                    0,
                    0
                );
                Ok(Connecting::new(handle, local, remote_endpoint))
            }
            Err(err) => {
                // connect failed — create new socket for Bound state
                NET_INTERFACE.remove_routed(handle);
                let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                let new_sock = Box::new(tcp::Socket::new(rx_buf, tx_buf));
                Err((Inner::Init(Init::Bound { socket: new_sock, local, pending_error: Some(err) }), err))
            }
        }
    }

    /// listen 操作：Unbound 先 auto-bind INADDR_ANY → 切换 Listen
    pub fn listen(self, backlog: usize) -> Result<Listening, (Self, SyscallErr)> {
        let (socket, local_endpoint, ver) = match self {
            Inner::Init(Init::Unbound(socket, ver)) => {
                let port = PortManager::alloc_ephemeral_port();
                let unspec = IpEndpoint::new(
                    match ver {
                        IpVersion::Ipv4 => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
                        IpVersion::Ipv6 => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED),
                    },
                    port,
                );
                (*socket, unspec, ver)
            }
            Inner::Init(Init::Bound { socket, local, pending_error: None }) => {
                let ver = match local.addr {
                    IpAddress::Ipv4(_) => IpVersion::Ipv4,
                    _ => IpVersion::Ipv6,
                };
                (*socket, local, ver)
            }
            other => return Err((other, SyscallErr::EINVAL)),
        };

        let listen_addr = if local_endpoint.addr.is_unspecified() {
            IpListenEndpoint::from(local_endpoint.port)
        } else {
            IpListenEndpoint::from(local_endpoint)
        };

        if listen_addr.port == 0 {
            return Err((
                Inner::Init(Init::Bound { socket: Box::new(socket), local: local_endpoint, pending_error: None }),
                SyscallErr::EINVAL,
            ));
        }

        if backlog > u16::MAX as usize {
            return Err((
                Inner::Init(Init::Bound { socket: Box::new(socket), local: local_endpoint, pending_error: None }),
                SyscallErr::EINVAL,
            ));
        }

        let listen_ifindex =
            crate::net::net_core::ifindex_for_local_addr(listen_addr.addr);

        let handle = NET_INTERFACE
            .add_routed_socket_on(InetProtocol::Tcp, socket, listen_ifindex)
            .ok_or_else(|| {
                let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                let new_sock = Box::new(tcp::Socket::new(rx_buf, tx_buf));
                (Inner::Init(Init::Bound { socket: new_sock, local: local_endpoint, pending_error: None }), SyscallErr::EAGAIN)
            })?;

        // 第一个 listen socket 用已有的 handle
        if let Err(e) = with_tcp_mut(handle, |socket| {
            socket.listen(listen_addr).map_err(|err| match err {
                tcp::ListenError::InvalidState => SyscallErr::EINVAL,
                tcp::ListenError::Unaddressable => SyscallErr::EINVAL,
            })
        })
        .unwrap_or(Err(SyscallErr::EINVAL))
        {
            NET_INTERFACE.remove_routed(handle);
            let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
            let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
            let new_sock = Box::new(tcp::Socket::new(rx_buf, tx_buf));
            return Err((
                Inner::Init(Init::Bound { socket: new_sock, local: local_endpoint, pending_error: None }),
                e,
            ));
        }

        // backlog：至少 1，最多 8
        let backlog = core::cmp::min(if backlog == 0 { 1 } else { backlog }, 8);
        let mut handles = vec![handle];

        // 补充额外 listen socket
        for _ in 1..backlog {
            let new_socket = {
                let rx_buf = SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
                let tx_buf = SocketBuffer::new(vec![0u8; LISTEN_BUFFER_SIZE]);
                let mut s = tcp::Socket::new(rx_buf, tx_buf);
                s.listen(listen_addr)
                    .map_err(|_| SyscallErr::EADDRINUSE)
                    .map_err(|e| {
                        let rx = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                        let tx = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                        (
                            Inner::Init(Init::Bound {
                                socket: Box::new(tcp::Socket::new(rx, tx)),
                                local: local_endpoint,
                                pending_error: None,
                            }),
                            e,
                        )
                    })?;
                s
            };
            if let Some(h) = NET_INTERFACE.add_routed_socket_on(InetProtocol::Tcp, new_socket, listen_ifindex) {
                handles.push(h);
            }
        }

        log::info!(
            "[TCP::listen] listening on {:?} with {} backlog sockets",
            listen_addr,
            handles.len()
        );

        Ok(Listening::new(handles, listen_addr))
    }

    /// accept 操作：从 Listening 中摘取一个已连接 socket
    pub fn accept(&mut self) -> Result<(Self, IpEndpoint), SyscallErr> {
        match self {
            Inner::Listening(listening) => {
                let (connected_handle, peer_endpoint) = listening.accept()?;
                let local = listening.local_endpoint();
                let connected = Established::new(connected_handle, local, peer_endpoint);
                let inner_connected = Inner::Established(connected);
                log::info!(
                    "[TCP::accept] accepted connection: local {:?} <-> peer {:?}",
                    local,
                    peer_endpoint
                );
                Ok((inner_connected, peer_endpoint))
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    /// shutdown 操作
    pub fn shutdown(&self, how: u32) -> GeneralRet<()> {
        match self {
            Inner::Established(e) => {
                log::info!(
                    "[Inner::shutdown] Established handle {}, how={}",
                    e.handle,
                    how
                );
                with_tcp_mut(e.handle, |socket| match how {
                    1 => {
                        log::info!(
                            "[Inner::shutdown] SHUT_WR: calling socket.close() (half-close)"
                        );
                        socket.close();
                    }
                    _ => {
                        log::info!("[Inner::shutdown] SHUT_RD/SHUT_RDWR: calling socket.abort()");
                        socket.abort();
                    }
                });
                Ok(())
            }
            Inner::SelfConnected(sc) => {
                if how == 1 || how == 2 {
                    sc.set_send_shutdown();
                }
                Ok(())
            }
            Inner::Listening(_) => {
                // listen socket 不允许 shutdown
                Err(SyscallErr::EINVAL)
            }
            Inner::Init(_) | Inner::Connecting(_) | Inner::Closed(_) => Err(SyscallErr::ENOTCONN),
        }
    }

    /// 设置 Nagle 启用
    pub fn set_nagle_enabled(&self, enabled: bool) {
        match self {
            Inner::Init(init) => match init {
                Init::Bound { .. } => {
                    // Lazy bind: settings applied at connect/listen time
                }
                Init::Unbound(_, _) => {
                    // Unbound socket 尚不可变访问 smoltcp，跳过（bind 后默认启用）
                }
            },
            Inner::Connecting(c) => {
                with_tcp_mut(c.handle, |s| s.set_nagle_enabled(enabled));
            }
            Inner::Listening(l) => {
                for &h in &l.handles {
                    with_tcp_mut(h, |s| s.set_nagle_enabled(enabled));
                }
            }
            Inner::Established(e) => {
                with_tcp_mut(e.handle, |s| s.set_nagle_enabled(enabled));
            }
            Inner::SelfConnected(_) => {}
            Inner::Closed(_) => {}
        }
    }

    /// 设置 Keep-Alive
    pub fn set_keep_alive(&self, enabled: bool) {
        let timeout = if enabled {
            Some(core::time::Duration::from_secs(7200).into())
        } else {
            None
        };
        match self {
            Inner::Init(init) => match init {
                Init::Bound { .. } => {
                    // Lazy bind: settings applied at connect/listen time
                }
                Init::Unbound(_, _) => {
                    // Unbound socket 尚不可变访问 smoltcp，跳过
                }
            },
            Inner::Connecting(c) => {
                with_tcp_mut(c.handle, |s| s.set_keep_alive(timeout));
            }
            Inner::Listening(l) => {
                for &h in &l.handles {
                    with_tcp_mut(h, |s| s.set_keep_alive(timeout));
                }
            }
            Inner::Established(e) => {
                with_tcp_mut(e.handle, |s| s.set_keep_alive(timeout));
            }
            Inner::SelfConnected(_) => {}
            Inner::Closed(_) => {}
        }
    }
}
