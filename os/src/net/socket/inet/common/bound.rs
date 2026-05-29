use smoltcp::iface::SocketHandle;
use smoltcp::wire::IpAddress;
use crate::net::net_core::DeviceEntry;

#[derive(Clone, Debug)]
pub struct BoundInner {
    pub socket_handle: Option<SocketHandle>,
    pub ifindex: u32,
    pub bound_addr: Option<IpAddress>,
    pub bound_port: u16,
}

impl BoundInner {
    pub fn new() -> Self {
        Self {
            socket_handle: None,
            ifindex: 0,
            bound_addr: None,
            bound_port: 0,
        }
    }

    pub fn with_iface(ifindex: u32) -> Self {
        Self {
            socket_handle: None,
            ifindex,
            bound_addr: None,
            bound_port: 0,
        }
    }

    pub fn bind(
        &mut self,
        handle: SocketHandle,
        ifindex: u32,
        addr: Option<IpAddress>,
        port: u16,
    ) {
        self.socket_handle = Some(handle);
        self.ifindex = ifindex;
        self.bound_addr = addr;
        self.bound_port = port;
    }

    pub fn bound_iface(&self) -> Option<DeviceEntry> {
        crate::net::net_core::find_by_index(self.ifindex)
    }

    pub fn is_bound(&self) -> bool {
        self.socket_handle.is_some()
    }
}
