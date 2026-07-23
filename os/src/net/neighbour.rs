//! Global neighbour table (ARP table) for the kernel.
//!
//! Entries are populated by intercepting ARP replies at the device receive path
//! (see [`crate::net::adapter`] consume methods).  The table is queried by
//! [`RTM_GETNEIGH`] netlink requests and exposed via `/proc/net/arp`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use smoltcp::wire::{EthernetAddress, IpAddress};
use spin::Mutex;

/// Neighbour entry state — simplified subset of Linux NUD_* states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighbourEntry {
    pub mac: EthernetAddress,
    /// NUD_REACHABLE (0x02) or NUD_PERMANENT (0x80).
    pub state: u16,
}

/// Neighbour Discovery Attribute types (linux/neighbour.h)
pub const NDA_UNSPEC: u16 = 0;
pub const NDA_DST: u16 = 1;
pub const NDA_LLADDR: u16 = 2;

/// Neighbour Unreachability Detection states (linux/neighbour.h subset)
pub const NUD_REACHABLE: u16 = 0x02;
pub const NUD_STALE: u16 = 0x04;
pub const NUD_PERMANENT: u16 = 0x80;

/// Global neighbour table keyed by (ifindex, IpAddress).
pub static NEIGHBOUR_TABLE: Mutex<BTreeMap<(u32, IpAddress), NeighbourEntry>> =
    Mutex::new(BTreeMap::new());

/// The ifindex of the device stack currently being polled by smoltcp.
/// Set by [`crate::net::config::NetInterface::poll_once`] before each stack poll.
/// Used by the adapter layer to tag received ARP replies with the correct ifindex.
pub static CURRENT_POLL_IFINDEX: Mutex<u32> = Mutex::new(0);

/// Record or refresh a neighbour entry.
pub fn neighbour_record(ifindex: u32, ip: IpAddress, mac: EthernetAddress) {
    // Only record unicast addresses.
    if !ip.is_unicast() {
        return;
    }
    let mut table = NEIGHBOUR_TABLE.lock();
    table.insert(
        (ifindex, ip),
        NeighbourEntry {
            mac,
            state: NUD_REACHABLE,
        },
    );
}

/// Delete a neighbour entry.  Returns `true` if an entry was removed.
pub fn neighbour_delete(ifindex: u32, ip: IpAddress) -> bool {
    let mut table = NEIGHBOUR_TABLE.lock();
    table.remove(&(ifindex, ip)).is_some()
}

/// Dump all current entries as a `Vec<(ifindex, IpAddress, EthernetAddress, state)>`.
pub fn neighbour_dump() -> Vec<(u32, IpAddress, EthernetAddress, u16)> {
    let table = NEIGHBOUR_TABLE.lock();
    table
        .iter()
        .map(|((ifidx, ip), entry)| (*ifidx, *ip, entry.mac, entry.state))
        .collect()
}

/// Try to intercept an ARP reply from a raw Ethernet frame.
/// If successful, records the mapping in the global neighbour table.
pub fn try_capture_arp_reply(frame_buf: &[u8], ifindex: u32) {
    use smoltcp::wire::{ArpOperation, ArpPacket, EthernetFrame, EthernetProtocol};

    let frame = match EthernetFrame::new_checked(frame_buf) {
        Ok(f) => f,
        Err(_) => return,
    };

    if frame.ethertype() != EthernetProtocol::Arp {
        return;
    }

    let arp = match ArpPacket::new_checked(frame.payload()) {
        Ok(a) => a,
        Err(_) => return,
    };

    if arp.operation() != ArpOperation::Reply {
        return;
    }

    let src_hw = arp.source_hardware_addr();
    let src_proto = arp.source_protocol_addr();

    if src_hw.len() < 6 || src_proto.len() < 4 {
        return;
    }

    let mac = EthernetAddress([
        src_hw[0], src_hw[1], src_hw[2], src_hw[3], src_hw[4], src_hw[5],
    ]);
    let ip = IpAddress::v4(src_proto[0], src_proto[1], src_proto[2], src_proto[3]);

    neighbour_record(ifindex, ip, mac);
}
