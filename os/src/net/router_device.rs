use alloc::sync::Arc;
use smoltcp::wire::{IpAddress, Ipv4Address, Ipv4Repr};

use super::iface::Iface;
use super::routing::Router;
use super::Mutex;

#[derive(Debug)]
pub enum NetError {
    NoRoute,
    TtlExceeded,
    SendFailed,
    ParseError,
}

pub trait RouterEnableDevice: Iface {
    fn route_and_send(&self, next_hop: Ipv4Address, ip_packet: &[u8]) -> Result<(), NetError>;

    fn is_my_ip(&self, addr: Ipv4Address) -> bool {
        self.ip_addrs()
            .iter()
            .any(|cidr| cidr.address() == IpAddress::Ipv4(addr))
    }

    fn netns_router(&self) -> Arc<Mutex<Router>>;

    fn handle_routable_packet(&self, ether_frame: &[u8]) -> Result<Option<Ipv4Repr>, NetError>;
}
