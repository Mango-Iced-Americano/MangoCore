//! 每个网络命名空间的端口 reservation 入口。

mod registry;

pub use registry::{AddressFamily, BindIntent, PortRegistry, PortReservation, TransportProtocol};

use crate::{
    net::{Endpoint, PSOCK, Socket},
    task::TaskControlBlock,
    utils::error::{SyscallErr, SyscallRet},
};
use alloc::sync::Arc;
use smoltcp::wire::{IpAddress, Ipv4Address};

/// 端口事务的外部兼容入口。
pub struct PortManager;

impl PortManager {
    /// 旧的内部 auto-bind 调用点暂时使用的候选器。
    /// 显式 bind 必须走 `bind_port()`，其 reservation 才是并发所有权的唯一来源。
    pub fn alloc_ephemeral_port() -> u16 {
        crate::net::net_core::current_netns()
            .ports
            .lock()
            .legacy_ephemeral()
            .unwrap_or(0)
    }

    /// 在调用者的 netns 内完成 reserve → bind → commit/abort。
    pub fn bind_port(
        task: &TaskControlBlock,
        socket: &Arc<dyn Socket>,
        endpoint: &Endpoint,
    ) -> SyscallRet {
        if !matches!(socket.socket_type(), PSOCK::Stream | PSOCK::Datagram) {
            return socket.bind(endpoint);
        }
        let namespace = task.process.net();
        let intent = socket.snapshot_bind_intent(endpoint)?;
        let reservation = {
            let mut ports = namespace.ports.lock();
            ports.reserve(&namespace, intent, socket)?
        };
        let actual = reservation.endpoint();

        match socket.bind(&actual) {
            Ok(value) => {
                let committed = namespace.ports.lock().commit(&reservation, socket);
                if let Err(error) = committed {
                    // 正常路径中持有 socket Arc，reservation 不会消失；此分支仅防御
                    // 内部所有权损坏，必须撤销 Reserved，不能留下永久占位。
                    namespace.ports.lock().abort(&reservation);
                    return Err(error);
                }
                socket.install_port_reservation(reservation);
                Ok(value)
            }
            Err(error) => {
                namespace.ports.lock().abort(&reservation);
                Err(error)
            }
        }
    }

    /// 兼容既有 ktest 清理入口；最多移除当前 netns 内的一个 owner。
    /// 生产关闭路径使用 reservation token，不得依赖此无 owner 的兼容接口。
    pub fn unregister_tcp_bind(port: u16) {
        crate::net::net_core::current_netns()
            .ports
            .lock()
            .release_one(PSOCK::Stream, port);
    }

    /// 兼容既有 ktest 清理入口；绝不删除整个 UDP bucket。
    pub fn unregister_udp_bind(port: u16) {
        crate::net::net_core::current_netns()
            .ports
            .lock()
            .release_one(PSOCK::Datagram, port);
    }

    /// 供遗留测试观察当前 netns 的 UDP 冲突结果。
    pub fn check_udp_conflict(
        port: u16,
        addr: Option<Ipv4Address>,
        reuse_addr: bool,
    ) -> Result<(), SyscallErr> {
        let intent = BindIntent::legacy_udp(port, addr.map(IpAddress::Ipv4), reuse_addr);
        crate::net::net_core::current_netns()
            .ports
            .lock()
            .check_conflict(&intent)
    }

    /// 供遗留测试观察当前 netns 的 TCP 冲突结果。
    pub fn check_tcp_conflict(port: u16, addr: Option<Ipv4Address>) -> bool {
        let intent = BindIntent::legacy_tcp(port, addr.map(IpAddress::Ipv4));
        crate::net::net_core::current_netns()
            .ports
            .lock()
            .check_conflict(&intent)
            .is_err()
    }
}
