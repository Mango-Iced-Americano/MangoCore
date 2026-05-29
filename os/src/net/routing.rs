use alloc::vec::Vec;
use log::debug;
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};

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

    /// Create a Router pre-populated with default routes:
    ///
    /// - `127.0.0.0/8` → ifindex 1 (lo), Connected
    /// - `10.0.2.0/24` → ifindex 2 (eth0), Connected (only if `NET_DEVICE` is present)
    /// - `0.0.0.0/0` → ifindex 2 (eth0), next-hop `10.0.2.2`, Default (only if `NET_DEVICE` is present)
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

        // Only add ethernet routes if a network device is present
        if crate::net::net_core::find_by_name("eth0").is_some() {
            // eth0 directly connected network
            router.add_route(
                IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 0)), 24),
                None,
                2, // eth0
                0,
                RouteType::Connected,
            );

            // Default gateway route
            router.add_route(
                IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(0, 0, 0, 0)), 0),
                Some(IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 2))),
                2,   // eth0
                100,
                RouteType::Default,
            );
        }

        router
    }
}
