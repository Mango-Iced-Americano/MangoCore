//! TCP 生命周期操作 —— bind / connect / listen / accept / shutdown

use crate::net::{
    config::{lookup_source_ip, NET_INTERFACE},
    socket::inet::common::PortManager,
    TCP_SOCKETS_TO_REMOVE,
};
use crate::trace_event;
use crate::utils::error::{GeneralRet, SyscallErr};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use smoltcp::{
    socket::tcp::{self, SocketBuffer},
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
                let socket = *socket;
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

                let handle = NET_INTERFACE
                    .add_socket(socket)
                    .ok_or_else(|| (Inner::Init(Init::new(ver)), SyscallErr::EAGAIN))?;

                Ok(Inner::Init(Init::Bound { handle, local }))
            }
            Inner::Init(Init::Bound { .. }) => {
                log::debug!("[TCP] already bound");
                Err((self, SyscallErr::EINVAL))
            }
            other => Err((other, SyscallErr::EINVAL)),
        }
    }

    /// connect 操作：Unbound 先 bind_ephemeral → 发起 SYN
    pub fn connect(self, remote_endpoint: IpEndpoint) -> Result<Connecting, (Self, SyscallErr)> {
        // 确保是 Init 状态
        let (handle, local) = match self {
            Inner::Init(Init::Unbound(_, ver)) => {
                // auto-bind to ephemeral
                let port = PortManager::alloc_ephemeral_port();
                let local_addr = lookup_source_ip(remote_endpoint.addr);
                let local = IpEndpoint::new(local_addr, port);
                let socket = {
                    let rx_buf = SocketBuffer::new(vec![0u8; DEFAULT_RX_BUF_SIZE]);
                    let tx_buf = SocketBuffer::new(vec![0u8; DEFAULT_TX_BUF_SIZE]);
                    tcp::Socket::new(rx_buf, tx_buf)
                };
                let handle = NET_INTERFACE
                    .add_socket(socket)
                    .ok_or_else(|| (Inner::Init(Init::new(ver)), SyscallErr::EAGAIN))?;
                (handle, local)
            }
            Inner::Init(Init::Bound { handle, local }) => {
                // 如果用户 bind 的是 INADDR_ANY（未指定地址），
                // 需要根据目标地址解析出源 IP（路由决策）
                let local = if local.addr.is_unspecified() {
                    IpEndpoint::new(lookup_source_ip(remote_endpoint.addr), local.port)
                } else {
                    local
                };
                (handle, local)
            }
            other => return Err((other, SyscallErr::EISCONN)),
        };

        let ret = NET_INTERFACE
            .inner_handler(|inner| {
                let socket = inner.sockets.get_mut::<tcp::Socket>(handle);
                let before_state = tcp_state_code(&socket.state());
                let ret = socket.connect(inner.iface.context(), remote_endpoint, local);
                let after_state = tcp_state_code(&socket.state());
                let ret_ok = ret.is_ok() as u64;
                trace_event!(
                    0xB020,
                    handle.as_usize() as u64,
                    before_state,
                    after_state,
                    ret_ok,
                    0,
                    0
                );
                ret
            })
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
                    handle.as_usize() as u64,
                    local.port as u64,
                    remote_endpoint.port as u64,
                    0,
                    0,
                    0
                );
                Ok(Connecting::new(handle, local, remote_endpoint))
            }
            Err(err) => Err((Inner::Init(Init::Bound { handle, local }), err)),
        }
    }

    /// listen 操作：Unbound 先 auto-bind INADDR_ANY → 切换 Listen
    pub fn listen(self, backlog: usize) -> Result<Listening, (Self, SyscallErr)> {
        // 如果未 bind，自动 bind INADDR_ANY:0
        let (handle, local_endpoint, ver) = match self {
            Inner::Init(Init::Unbound(socket, ver)) => {
                let port = PortManager::alloc_ephemeral_port();
                let unspec = IpEndpoint::new(
                    match ver {
                        IpVersion::Ipv4 => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
                        IpVersion::Ipv6 => IpAddress::Ipv6(smoltcp::wire::Ipv6Address::UNSPECIFIED),
                    },
                    port,
                );
                let socket = *socket;
                let handle = NET_INTERFACE
                    .add_socket(socket)
                    .ok_or_else(|| (Inner::Init(Init::new(ver)), SyscallErr::EAGAIN))?;
                (handle, unspec, ver)
            }
            Inner::Init(Init::Bound { handle, local }) => {
                let ver = match local.addr {
                    IpAddress::Ipv4(_) => IpVersion::Ipv4,
                    IpAddress::Ipv6(_) => IpVersion::Ipv6,
                };
                (handle, local, ver)
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
                Inner::Init(Init::Bound {
                    handle,
                    local: local_endpoint,
                }),
                SyscallErr::EINVAL,
            ));
        }

        if backlog > u16::MAX as usize {
            return Err((
                Inner::Init(Init::Bound {
                    handle,
                    local: local_endpoint,
                }),
                SyscallErr::EINVAL,
            ));
        }

        // 第一个 listen socket 用已有的 handle
        if let Err(e) = with_tcp_mut(handle, |socket| {
            socket.listen(listen_addr).map_err(|err| match err {
                tcp::ListenError::InvalidState => SyscallErr::EINVAL,
                tcp::ListenError::Unaddressable => SyscallErr::EINVAL,
            })
        })
        .unwrap_or(Err(SyscallErr::EINVAL))
        {
            return Err((
                Inner::Init(Init::Bound {
                    handle,
                    local: local_endpoint,
                }),
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
                        (
                            Inner::Init(Init::Bound {
                                handle,
                                local: local_endpoint,
                            }),
                            e,
                        )
                    })?;
                s
            };
            if let Some(h) = NET_INTERFACE.add_socket(new_socket) {
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
                NET_INTERFACE.poll();
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
                Init::Bound { handle, .. } => {
                    with_tcp_mut(*handle, |s| s.set_nagle_enabled(enabled));
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
                Init::Bound { handle, .. } => {
                    with_tcp_mut(*handle, |s| s.set_keep_alive(timeout));
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
