use alloc::fmt;
use alloc::vec::Vec;
use log::debug;
use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use crate::utils::error::SyscallErr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteSocketHandle(pub(crate) usize);

impl fmt::Display for RouteSocketHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RH({})", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InetProtocol {
    Tcp,
    Udp,
    Raw,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SocketBinding {
    pub ifindex: u32,
    pub handle: SocketHandle,
    pub proto: InetProtocol,
}

#[derive(Clone, Debug)]
pub struct RouteDecision {
    pub ifindex: u32,
    pub source: IpAddress,
    pub next_hop: Option<IpAddress>,
    pub is_local: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RouteKind {
    Local { dst_ifindex: u32 },
    Connected { oif: u32 },
    Gateway { oif: u32, gw: Ipv4Address },
    Unreachable,
}

/// The type of a routing table entry.
#[derive(Clone, Debug, PartialEq)]
pub enum RouteType {
    /// Directly connected network.
    Connected,
    /// Static route (administratively configured).
    Static,
    /// Default route (0.0.0.0/0 or ::/0).
    Default,
}

/// A single entry in the routing table.
#[derive(Clone, Debug)]
pub struct RouteEntry {
    /// Destination CIDR.
    pub destination: IpCidr,
    /// Next-hop IP address (None for connected routes).
    pub next_hop: Option<IpAddress>,
    /// Output interface index.
    pub ifindex: u32,
    /// Route metric (lower is preferred).
    pub metric: u32,
    /// Type of the route.
    pub route_type: RouteType,
}

/// A simple routing table.
#[derive(Clone, Debug)]
pub struct RouteTable {
    pub entries: Vec<RouteEntry>,
}

impl RouteTable {
    /// Create an empty routing table.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add a route entry.
    pub fn add(&mut self, entry: RouteEntry) {
        self.entries.push(entry);
    }

    /// Remove all route entries matching the given destination.
    pub fn remove(&mut self, destination: &IpCidr) {
        self.entries.retain(|e| &e.destination != destination);
    }
}

/// A routing table wrapper providing route lookup and management.
#[derive(Clone, Debug)]
pub struct Router {
    pub(crate) table: RouteTable,
}

impl Router {
    /// Create a new empty Router.
    pub fn new() -> Self {
        Self {
            table: RouteTable::new(),
        }
    }

    /// Add a route entry to the routing table.
    pub fn add_route(
        &mut self,
        dest: IpCidr,
        next_hop: Option<IpAddress>,
        ifindex: u32,
        metric: u32,
        route_type: RouteType,
    ) {
        self.table.add(RouteEntry {
            destination: dest,
            next_hop,
            ifindex,
            metric,
            route_type,
        });
    }

    /// Remove all route entries matching the given destination CIDR.
    pub fn remove_route(&mut self, dest: &IpCidr) {
        self.table.remove(dest);
    }

    /// Look up the best matching route for the given destination IP.
    ///
    /// Uses longest prefix match: among all routes whose CIDR contains `dest_ip`,
    /// returns the one with the greatest prefix length (most specific).
    pub fn lookup_route(&self, dest_ip: Ipv4Address) -> Option<&RouteEntry> {
        let ip = IpAddress::Ipv4(dest_ip);
        let mut best_entry: Option<&RouteEntry> = None;
        let mut best_prefix_len: Option<u8> = None;

        for entry in &self.table.entries {
            if entry.destination.contains_addr(&ip) {
                let prefix_len = entry.destination.prefix_len();
                if best_prefix_len.map_or(true, |best| prefix_len > best) {
                    best_prefix_len = Some(prefix_len);
                    best_entry = Some(entry);
                }
            }
        }

        if let Some(entry) = best_entry {
            debug!(
                "route_lookup: dst={} -> ifindex={} next_hop={:?} route_type={:?}",
                dest_ip, entry.ifindex, entry.next_hop, entry.route_type
            );
        } else {
            debug!("route_lookup: dst={} -> no route found", dest_ip);
        }

        best_entry
    }

    pub fn lookup_route_owned(&self, dest_ip: Ipv4Address) -> Option<RouteEntry> {
        self.lookup_route(dest_ip).cloned()
    }

    /// Create a Router pre-populated with default routes.
    /// Dynamic: uses DHCP-assigned IP/gateway from net_core instead of hardcoded values.
    pub fn init_default() -> Self {
        let mut router = Self::new();

        // Loopback route
        router.add_route(
            IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 0)), 8),
            None,
            1, // lo
            0,
            RouteType::Connected,
        );

        if let Some(cidr) = crate::net::net_core::eth0_ipv4_cidr() {
            // Connected route from DHCP CIDR
            router.add_route(
                IpCidr::new(cidr.address(), cidr.prefix_len()),
                None,
                2, // eth0
                0,
                RouteType::Connected,
            );

            if let Some(gw) = crate::net::net_core::default_gateway() {
                router.add_route(
                    IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(0, 0, 0, 0)), 0),
                    Some(IpAddress::Ipv4(gw)),
                    2,   // eth0
                    100,
                    RouteType::Default,
                );
            }
        }

        router
    }
}

pub fn route_output(dest: IpAddress) -> Result<RouteDecision, SyscallErr> {
    let router = Router::init_default();
    match dest {
        IpAddress::Ipv4(addr) => {
            let is_local = crate::net::net_core::IFACES
                .lock()
                .iter()
                .any(|d| d.ip_addrs.iter().any(|c| c.address() == dest));
            if is_local {
                let ifaces = crate::net::net_core::IFACES.lock();
                let dst_ifindex = ifaces
                    .iter()
                    .find(|d| d.ip_addrs.iter().any(|c| c.address() == dest))
                    .map(|d| d.ifindex)
                    .unwrap_or(1);
                let source = ifaces
                    .iter()
                    .find(|d| d.ifindex == dst_ifindex)
                    .and_then(|d| d.ip_addrs.first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(127, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: dst_ifindex,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }

            if addr.0[0] == 127 {
                let source = crate::net::net_core::loopback_iface()
                    .and_then(|d| d.ip_addrs.first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(127, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: 1,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }

            if let Some(entry) = router.lookup_route_owned(addr) {
                let source = crate::net::net_core::find_by_index(entry.ifindex)
                    .and_then(|d| d.ip_addrs.first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(0, 0, 0, 0));
                return Ok(RouteDecision {
                    ifindex: entry.ifindex,
                    source,
                    next_hop: entry.next_hop,
                    is_local: false,
                });
            }

            Err(SyscallErr::ENETUNREACH)
        }
        IpAddress::Ipv6(_) => {
            if crate::net::net_core::find_by_name("eth0").is_some() {
                Ok(RouteDecision {
                    ifindex: 2,
                    source: IpAddress::v4(0, 0, 0, 0),
                    next_hop: None,
                    is_local: false,
                })
            } else {
                Err(SyscallErr::ENETUNREACH)
            }
        }
    }
}
