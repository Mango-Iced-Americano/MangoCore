use alloc::fmt;
use alloc::vec::Vec;
use log::debug;
use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

use super::Mutex;
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

    /// Remove connected routes for (ifindex, destination) only.
    pub fn remove_connected(&mut self, ifindex: u32, dest: &IpCidr) {
        self.entries.retain(|e| {
            e.ifindex != ifindex || e.destination != *dest || e.route_type != RouteType::Connected
        });
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

    /// Atomically replace routes owned by the eth0 DHCP lease.
    pub fn replace_dhcp_ipv4(
        &mut self,
        ifindex: u32,
        cidr: Option<IpCidr>,
        gateway: Option<Ipv4Address>,
    ) {
        self.table.entries.retain(|entry| {
            entry.ifindex != ifindex
                || !matches!(entry.route_type, RouteType::Connected | RouteType::Default)
        });

        if let Some(cidr) = cidr {
            let network = match cidr {
                IpCidr::Ipv4(cidr) => IpCidr::Ipv4(cidr.network()),
                IpCidr::Ipv6(cidr) => IpCidr::Ipv6(cidr),
            };
            self.add_route(network, None, ifindex, 0, RouteType::Connected);
            if let Some(gateway) = gateway {
                self.add_route(
                    IpCidr::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0),
                    Some(IpAddress::Ipv4(gateway)),
                    ifindex,
                    100,
                    RouteType::Default,
                );
            }
        }
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

    /// Fill the router with default routes (loopback + DHCP).
    /// Safe to call multiple times — existing entries are not duplicated
    /// because the caller is expected to manage the table lifecycle.
    pub fn fill_default(&mut self) {
        // Look up eth0 ifindex dynamically from the current netns
        let eth0_ifindex = crate::net::net_core::find_by_name("eth0")
            .map(|d| d.ifindex)
            .unwrap_or(2);

        // Loopback route
        self.add_route(
            IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(127, 0, 0, 0)), 8),
            None,
            1, // lo
            0,
            RouteType::Connected,
        );

        if let Some(cidr) = crate::net::net_core::eth0_ipv4_cidr() {
            // Connected route from DHCP CIDR
            let network = match cidr {
                IpCidr::Ipv4(cidr) => IpCidr::Ipv4(cidr.network()),
                IpCidr::Ipv6(cidr) => IpCidr::Ipv6(cidr),
            };
            self.add_route(
                network,
                None,
                eth0_ifindex,
                0,
                RouteType::Connected,
            );

            if let Some(gw) = crate::net::net_core::default_gateway() {
                self.add_route(
                    IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(0, 0, 0, 0)), 0),
                    Some(IpAddress::Ipv4(gw)),
                    eth0_ifindex,
                    100,
                    RouteType::Default,
                );
            }
        }
    }

    /// Fill default routes into the current netns router.
    /// Should be called once during network init (after DHCP info is available).
    pub fn init_router() {
        crate::net::net_core::current_netns()
            .router
            .lock()
            .fill_default();
    }
}

pub fn route_output(dest: IpAddress) -> Result<RouteDecision, SyscallErr> {
    let ns = crate::net::net_core::current_netns();

    // Lazily populate the netns router with default routes on first use.
    {
        let mut router = ns.router.lock();
        if router.table.entries.is_empty() {
            router.fill_default();
        }
    }

    match dest {
        IpAddress::Ipv4(addr) => {
            let list = ns.device_list.lock();
            let is_local = list
                .values()
                .any(|iface| iface.ip_addrs().iter().any(|c| c.address() == dest));
            if is_local {
                let dst_ifindex = list
                    .values()
                    .find(|iface| iface.ip_addrs().iter().any(|c| c.address() == dest))
                    .map(|iface| iface.nic_id() as u32)
                    .unwrap_or(1);
                let source = list
                    .values()
                    .find(|iface| iface.nic_id() as u32 == dst_ifindex)
                    .and_then(|iface| iface.ip_addrs().first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(127, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: dst_ifindex,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }

            drop(list);

            if addr.0[0] == 127 {
                let source = crate::net::net_core::loopback_iface()
                    .and_then(|d| d.iface.ip_addrs().first().map(|c| c.address()))
                    .unwrap_or(IpAddress::v4(127, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: 1,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }

            if let Some(entry) = ns.router.lock().lookup_route_owned(addr) {
                let source = crate::net::net_core::find_by_index(entry.ifindex)
                    .and_then(|d| d.iface.ip_addrs().first().map(|c| c.address()))
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
        IpAddress::Ipv6(addr) => {
            // 先查本机接口是否有这个 v6 地址
            let list = ns.device_list.lock();
            let is_local = list
                .values()
                .any(|iface| iface.ip_addrs().iter().any(|c| c.address() == dest));
            if is_local {
                let dst_ifindex = list
                    .values()
                    .find(|iface| iface.ip_addrs().iter().any(|c| c.address() == dest))
                    .map(|iface| iface.nic_id() as u32)
                    .unwrap_or(1);
                let source = list
                    .values()
                    .find(|iface| iface.nic_id() as u32 == dst_ifindex)
                    .and_then(|iface| {
                        iface.ip_addrs().iter().find_map(|c| {
                            if let IpAddress::Ipv6(_) = c.address() {
                                Some(c.address())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: dst_ifindex,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }
            drop(list);

            // ::1 loopback
            if addr == smoltcp::wire::Ipv6Address::LOOPBACK {
                let source = crate::net::net_core::loopback_iface()
                    .and_then(|d| {
                        d.iface.ip_addrs().iter().find_map(|c| {
                            if let IpAddress::Ipv6(_) = c.address() {
                                Some(c.address())
                            } else {
                                None
                            }
                        })
                    })
                    .unwrap_or(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 1));
                return Ok(RouteDecision {
                    ifindex: 1,
                    source,
                    next_hop: None,
                    is_local: true,
                });
            }

            // 查 v6 路由表
            let router = ns.router.lock();
            for entry in &router.table.entries {
                if entry.destination.contains_addr(&dest) {
                    let source = crate::net::net_core::find_by_index(entry.ifindex)
                        .and_then(|d| {
                            d.iface.ip_addrs().iter().find_map(|c| {
                                if let IpAddress::Ipv6(_) = c.address() {
                                    Some(c.address())
                                } else {
                                    None
                                }
                            })
                        })
                        .unwrap_or(IpAddress::v6(0, 0, 0, 0, 0, 0, 0, 0));
                    return Ok(RouteDecision {
                        ifindex: entry.ifindex,
                        source,
                        next_hop: entry.next_hop,
                        is_local: false,
                    });
                }
            }

            Err(SyscallErr::ENETUNREACH)
        }
    }
}
