//! 每个网络命名空间的端口 reservation 入口。

mod registry;

pub use registry::{AddressFamily, BindIntent, PortRegistry, PortReservation, TransportProtocol};

use crate::{
    net::{Endpoint, Socket, PSOCK},
    task::TaskControlBlock,
    utils::error::{SyscallErr, SyscallRet},
};
use alloc::sync::Arc;

/// 自动绑定的触发语义；端点选择由具体 INET socket 在锁外完成。
#[derive(Clone, Copy)]
pub enum AutoBindPurpose {
    Connect,
    Listen,
    Send,
}

/// 端口事务的外部入口。
pub struct PortManager;

impl PortManager {
    /// 为尚未绑定的 INET socket 选择端点，并复用显式 bind 的完整端口事务。
    ///
    /// 端点推导不持有 PortRegistry；`bind_port()` 再按 N0 reserve -> N1/N2 bind
    /// -> N0 commit/abort 执行。因此两个 CPU 同时看到未绑定时，失败者只会撤销
    /// 自己的 Reserved owner，不能把临时端口暴露为无 owner 的候选值。
    pub fn ensure_auto_bound(
        task: &TaskControlBlock,
        socket: &Arc<dyn Socket>,
        peer: Option<&Endpoint>,
        purpose: AutoBindPurpose,
    ) -> Result<(), SyscallErr> {
        if let Some(endpoint) = socket.auto_bind_endpoint(peer, purpose)? {
            Self::bind_port(task, socket, &endpoint)?;
        }
        Ok(())
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
}
