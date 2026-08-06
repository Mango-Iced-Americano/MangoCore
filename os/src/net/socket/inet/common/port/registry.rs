use crate::{
    net::{Endpoint, Socket},
    task::NetNamespace,
    utils::error::SyscallErr,
};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address, Ipv6Address};

const EPHEMERAL_PORT_MIN: u16 = 49_152;
const EPHEMERAL_PORT_MAX: u16 = 65_534;

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub enum AddressFamily {
    Ipv4,
    Ipv6,
}

/// 一次 bind 事务在取得 socket 内部锁之前冻结的冲突判定输入。
///
/// `reserve()` 只消费这份不可变快照，因此 registry 锁不会与 socket 锁嵌套。
#[derive(Clone)]
pub struct BindIntent {
    pub protocol: TransportProtocol,
    pub family: AddressFamily,
    pub address: Option<IpAddress>,
    pub port: u16,
    pub ifindex: Option<u32>,
    pub reuse_addr: bool,
    pub reuse_port: bool,
    pub ipv6_v6only: bool,
}

impl BindIntent {
    pub fn inet(
        protocol: TransportProtocol,
        family: AddressFamily,
        address: Option<IpAddress>,
        port: u16,
        ifindex: Option<u32>,
        reuse_addr: bool,
        ipv6_v6only: bool,
    ) -> Self {
        Self {
            protocol,
            family,
            address,
            port,
            ifindex,
            reuse_addr,
            reuse_port: false,
            ipv6_v6only,
        }
    }

    pub fn endpoint(&self) -> Endpoint {
        let address = self.address.unwrap_or(match self.family {
            AddressFamily::Ipv4 => IpAddress::Ipv4(Ipv4Address::UNSPECIFIED),
            AddressFamily::Ipv6 => IpAddress::Ipv6(Ipv6Address::UNSPECIFIED),
        });
        Endpoint::Ip(IpEndpoint::new(address, self.port))
    }
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct PortKey {
    protocol: TransportProtocol,
    family: AddressFamily,
    address: Option<IpAddress>,
    port: u16,
    ifindex: Option<u32>,
}

impl From<&BindIntent> for PortKey {
    fn from(intent: &BindIntent) -> Self {
        Self {
            protocol: intent.protocol,
            family: intent.family,
            address: intent.address,
            port: intent.port,
            ifindex: intent.ifindex,
        }
    }
}

impl PortKey {
    fn endpoint(&self) -> Endpoint {
        BindIntent {
            protocol: self.protocol,
            family: self.family,
            address: self.address,
            port: self.port,
            ifindex: self.ifindex,
            reuse_addr: false,
            reuse_port: false,
            ipv6_v6only: false,
        }
        .endpoint()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PortOwnerState {
    /// Registry 已占位，但 socket 自身的 bind 尚未提交。
    Reserved,
    /// Socket bind 与 registry owner 已共同发布。
    Bound,
}

struct PortOwner {
    token: u64,
    socket: Weak<dyn Socket>,
    state: PortOwnerState,
    reuse_addr: bool,
    reuse_port: bool,
    ipv6_v6only: bool,
}

/// 一个端口 owner 的精确释放凭据。
///
/// `token + Weak<Socket>` 共同标识 owner，避免同端口的 reuse peer 被误删。
pub struct PortReservation {
    namespace: Weak<NetNamespace>,
    key: PortKey,
    token: u64,
    owner: Weak<dyn Socket>,
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        // Reservation 是唯一凭据；无论 bind 中途失败还是 socket 正常析构，
        // 最终 drop 都会精确撤销自己的 owner。重复 abort/release 是幂等的。
        self.release();
    }
}

impl PortReservation {
    pub fn endpoint(&self) -> Endpoint {
        self.key.endpoint()
    }

    pub fn release(&self) {
        if let Some(namespace) = self.namespace.upgrade() {
            namespace.ports.lock().release(self);
        }
    }
}

pub struct PortRegistry {
    /// 下一次临时端口搜索的起点；始终在 registry 锁内推进。
    next_ephemeral: u16,
    /// 单调 owner 身份，禁止在本次启动中复用。
    next_token: u64,
    /// 一个 key 可容纳多个满足 reuse 规则的 owner。
    buckets: BTreeMap<PortKey, Vec<PortOwner>>,
}

impl PortRegistry {
    pub fn new() -> Self {
        Self {
            next_ephemeral: EPHEMERAL_PORT_MIN,
            next_token: 1,
            buckets: BTreeMap::new(),
        }
    }

    pub fn reserve(
        &mut self,
        namespace: &Arc<NetNamespace>,
        mut intent: BindIntent,
        socket: &Arc<dyn Socket>,
    ) -> Result<PortReservation, SyscallErr> {
        self.prune_dead_owners();
        if intent.port == 0 {
            intent.port = self.select_ephemeral(&intent)?;
        }
        self.check_conflict(&intent)?;
        let token = self.allocate_token()?;
        let key = PortKey::from(&intent);
        let owner = Arc::downgrade(socket);
        self.buckets
            .entry(key.clone())
            .or_default()
            .push(PortOwner {
                token,
                socket: owner.clone(),
                state: PortOwnerState::Reserved,
                reuse_addr: intent.reuse_addr,
                reuse_port: intent.reuse_port,
                ipv6_v6only: intent.ipv6_v6only,
            });
        Ok(PortReservation {
            namespace: Arc::downgrade(namespace),
            key,
            token,
            owner,
        })
    }

    pub fn commit(
        &mut self,
        reservation: &PortReservation,
        socket: &Arc<dyn Socket>,
    ) -> Result<(), SyscallErr> {
        let owners = self
            .buckets
            .get_mut(&reservation.key)
            .ok_or(SyscallErr::EADDRNOTAVAIL)?;
        let owner = owners
            .iter_mut()
            .find(|owner| {
                owner.token == reservation.token
                    && owner.socket.ptr_eq(&reservation.owner)
                    && owner.socket.ptr_eq(&Arc::downgrade(socket))
            })
            .ok_or(SyscallErr::EADDRNOTAVAIL)?;
        owner.state = PortOwnerState::Bound;
        Ok(())
    }

    pub fn abort(&mut self, reservation: &PortReservation) {
        self.remove_matching(reservation);
    }
    pub fn release(&mut self, reservation: &PortReservation) {
        self.remove_matching(reservation);
    }

    pub fn check_conflict(&self, intent: &BindIntent) -> Result<(), SyscallErr> {
        for (key, owners) in &self.buckets {
            if key.protocol != intent.protocol || key.port != intent.port {
                continue;
            }
            if owners.iter().any(|owner| {
                matches!(
                    owner.state,
                    PortOwnerState::Reserved | PortOwnerState::Bound
                ) && self.keys_overlap(key, intent, owner)
                    && !reuse_compatible(intent, owner)
            }) {
                return Err(SyscallErr::EADDRINUSE);
            }
        }
        Ok(())
    }

    fn select_ephemeral(&mut self, intent: &BindIntent) -> Result<u16, SyscallErr> {
        let start = self
            .next_ephemeral
            .clamp(EPHEMERAL_PORT_MIN, EPHEMERAL_PORT_MAX);
        let mut port = start;
        loop {
            let mut candidate = intent.clone();
            candidate.port = port;
            if self.check_conflict(&candidate).is_ok() {
                self.next_ephemeral = if port == EPHEMERAL_PORT_MAX {
                    EPHEMERAL_PORT_MIN
                } else {
                    port + 1
                };
                return Ok(port);
            }
            port = if port == EPHEMERAL_PORT_MAX {
                EPHEMERAL_PORT_MIN
            } else {
                port + 1
            };
            if port == start {
                return Err(SyscallErr::ENOSPC);
            }
        }
    }

    fn allocate_token(&mut self) -> Result<u64, SyscallErr> {
        let token = self.next_token;
        self.next_token = self.next_token.checked_add(1).ok_or(SyscallErr::ENOSPC)?;
        Ok(token)
    }

    fn prune_dead_owners(&mut self) {
        for owners in self.buckets.values_mut() {
            owners.retain(|owner| owner.socket.upgrade().is_some());
        }
        self.buckets.retain(|_, owners| !owners.is_empty());
    }

    fn remove_matching(&mut self, reservation: &PortReservation) {
        if let Some(owners) = self.buckets.get_mut(&reservation.key) {
            owners.retain(|owner| {
                owner.token != reservation.token || !owner.socket.ptr_eq(&reservation.owner)
            });
        }
        self.buckets.retain(|_, owners| !owners.is_empty());
    }

    fn keys_overlap(&self, key: &PortKey, intent: &BindIntent, owner: &PortOwner) -> bool {
        if !ifindex_overlap(key.ifindex, intent.ifindex) {
            return false;
        }
        if key.family == intent.family {
            return address_overlap(key.address, intent.address);
        }
        let (v6_key, v6_intent) = if key.family == AddressFamily::Ipv6 {
            (key.address.is_none(), owner.ipv6_v6only)
        } else {
            (intent.address.is_none(), intent.ipv6_v6only)
        };
        v6_key && !v6_intent
    }
}

fn address_overlap(left: Option<IpAddress>, right: Option<IpAddress>) -> bool {
    left.is_none() || right.is_none() || left == right
}
fn ifindex_overlap(left: Option<u32>, right: Option<u32>) -> bool {
    left.is_none() || right.is_none() || left == right
}
fn reuse_compatible(intent: &BindIntent, owner: &PortOwner) -> bool {
    (intent.reuse_addr && owner.reuse_addr) || (intent.reuse_port && owner.reuse_port)
}
