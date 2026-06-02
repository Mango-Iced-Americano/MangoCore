use alloc::vec;
use alloc::vec::Vec;

// ── Address families ──
/// AF_NETLINK (from linux/socket.h)
pub const AF_NETLINK: u16 = 16;

// ── NLMSG alignment ──
/// Alignment for netlink messages (4 bytes)
pub const NLMSG_ALIGNTO: usize = 4;

/// Align `len` up to NLMSG_ALIGNTO boundary.
pub fn nlmsg_align(len: usize) -> usize {
    (len + NLMSG_ALIGNTO - 1) & !(NLMSG_ALIGNTO - 1)
}

// ── Netlink message types (linux/netlink.h) ──
/// No operation
pub const NLMSG_NOOP: u16 = 1;
/// Error (payload is nlmsgerr)
pub const NLMSG_ERROR: u16 = 2;
/// End of a multipart message
pub const NLMSG_DONE: u16 = 3;
/// Data lost
pub const NLMSG_OVERRUN: u16 = 4;
/// Lowest type number for application-specific messages
pub const NLMSG_MIN_TYPE: u16 = 0x10;

// ── NLM_F flags (linux/netlink.h) ──
/// Request (must be set for all requests)
pub const NLM_F_REQUEST: u16 = 0x01;
/// Multipart message (terminated by NLMSG_DONE)
pub const NLM_F_MULTI: u16 = 0x02;
/// Reply with ack (nlmsgerr with errno=0 on success)
pub const NLM_F_ACK: u16 = 0x04;
/// Echo request (suppress normal response, emit notification)
pub const NLM_F_ECHO: u16 = 0x08;
/// Return the complete table (not just active entries) — used with GET
pub const NLM_F_ROOT: u16 = 0x100;
/// Return all matching entries — used with GET
pub const NLM_F_MATCH: u16 = 0x200;
/// Atomic operation — used with GET
pub const NLM_F_ATOMIC: u16 = 0x400;
/// Convenience: NLM_F_ROOT | NLM_F_MATCH (flags for dump request)
pub const NLM_F_DUMP: u16 = NLM_F_ROOT | NLM_F_MATCH;

// ── NLM_F flags for NEW / SET (same bit positions, different semantics) ──
/// Replace existing entry
pub const NLM_F_REPLACE: u16 = 0x100;
/// Do not create, error if exists
pub const NLM_F_EXCL: u16 = 0x200;
/// Create entry if it does not exist
pub const NLM_F_CREATE: u16 = 0x400;
/// Add at end of list
pub const NLM_F_APPEND: u16 = 0x800;

// ── NLA attribute flags (linux/netlink.h) ──
/// Attribute is nested (contains sub-attributes)
pub const NLA_F_NESTED: u16 = 1 << 15;       // 0x8000
/// Attribute payload is in network byte order
pub const NLA_F_NET_BYTEORDER: u16 = 1 << 14; // 0x4000

// ── RTM types (linux/rtnetlink.h) ──
// Link
/// Add network link
pub const RTM_NEWLINK: u16 = 16;
/// Delete network link
pub const RTM_DELLINK: u16 = 17;
/// Get network link configuration
pub const RTM_GETLINK: u16 = 18;
/// Set network link configuration
pub const RTM_SETLINK: u16 = 19;
// Address
/// Add network address
pub const RTM_NEWADDR: u16 = 20;
/// Delete network address
pub const RTM_DELADDR: u16 = 21;
/// Get network address configuration
pub const RTM_GETADDR: u16 = 22;
// Route
/// Add route
pub const RTM_NEWROUTE: u16 = 24;
/// Delete route
pub const RTM_DELROUTE: u16 = 25;
/// Get route table
pub const RTM_GETROUTE: u16 = 26;
// Neighbour
/// Add neighbour table entry
pub const RTM_NEWNEIGH: u16 = 28;
/// Delete neighbour table entry
pub const RTM_DELNEIGH: u16 = 29;
/// Get neighbour table
pub const RTM_GETNEIGH: u16 = 30;
// Rule
/// Add routing rule
pub const RTM_NEWRULE: u16 = 32;
/// Delete routing rule
pub const RTM_DELRULE: u16 = 33;
/// Get routing rule table
pub const RTM_GETRULE: u16 = 34;

// ── IFLA constants (linux/if_link.h) ──
/// Unspecified (padding)
pub const IFLA_UNSPEC: u16 = 0;
/// Interface L2 address
pub const IFLA_ADDRESS: u16 = 1;
/// Interface broadcast address
pub const IFLA_BROADCAST: u16 = 2;
/// Interface name (NUL-terminated string)
pub const IFLA_IFNAME: u16 = 3;
/// Interface MTU
pub const IFLA_MTU: u16 = 4;
/// Link type (index of underlying interface, e.g. VLAN parent)
pub const IFLA_LINK: u16 = 5;
/// Queueing discipline
pub const IFLA_QDISC: u16 = 6;
/// Interface statistics (32-bit counters)
pub const IFLA_STATS: u16 = 7;
/// Path cost
pub const IFLA_COST: u16 = 8;
/// Interface priority
pub const IFLA_PRIORITY: u16 = 9;
/// Master (bridge/bond) interface index
pub const IFLA_MASTER: u16 = 10;
/// Wireless extensions
pub const IFLA_WIRELESS: u16 = 11;
/// Protocol-specific info (e.g. spanning tree per-port)
pub const IFLA_PROTINFO: u16 = 12;
/// Interface transmit queue length
pub const IFLA_TXQLEN: u16 = 13;
/// Device-level HW address map
pub const IFLA_MAP: u16 = 14;
/// Interface weight
pub const IFLA_WEIGHT: u16 = 15;
/// Interface operational state (IF_OPER_*)
pub const IFLA_OPERSTATE: u16 = 16;
/// Interface link mode
pub const IFLA_LINKMODE: u16 = 17;
/// Link info nested attribute (contains IFLA_INFO_KIND, IFLA_INFO_DATA, etc.)
pub const IFLA_LINKINFO: u16 = 18;
/// PID of network namespace (target ns for move)
pub const IFLA_NET_NS_PID: u16 = 19;
/// Interface alias (text name)
pub const IFLA_IFALIAS: u16 = 20;
/// Number of SR-IOV virtual functions
pub const IFLA_NUM_VF: u16 = 21;
/// VF info list nested attribute
pub const IFLA_VFINFO_LIST: u16 = 22;
/// Interface statistics (64-bit counters)
pub const IFLA_STATS64: u16 = 23;
/// VF ports nested attribute
pub const IFLA_VF_PORTS: u16 = 24;
/// Port self nested attribute
pub const IFLA_PORT_SELF: u16 = 25;
/// AF-specific attribute block
pub const IFLA_AF_SPEC: u16 = 26;
/// Interface group
pub const IFLA_GROUP: u16 = 27;
/// FD referring to target network namespace
pub const IFLA_NET_NS_FD: u16 = 28;
/// Extended mask (bitmask of IFLA_EXT_*)
pub const IFLA_EXT_MASK: u16 = 29;
/// Promiscuity count
pub const IFLA_PROMISCUITY: u16 = 30;
/// Number of TX queues
pub const IFLA_NUM_TX_QUEUES: u16 = 31;
/// Number of RX queues
pub const IFLA_NUM_RX_QUEUES: u16 = 32;
/// Carrier state (0=down, 1=up)
pub const IFLA_CARRIER: u16 = 33;
/// Physical port identifier (OID)
pub const IFLA_PHYS_PORT_ID: u16 = 34;
/// Carrier up/down changes counter
pub const IFLA_CARRIER_CHANGES: u16 = 35;
/// Physical switch identifier (OID)
pub const IFLA_PHYS_SWITCH_ID: u16 = 36;
/// Link netns ID for cross-netns references
pub const IFLA_LINK_NETNSID: u16 = 37;
/// Protocol-down reason
pub const IFLA_PROTO_DOWN: u16 = 38;
/// Maximum GSO segment count
pub const IFLA_GSO_MAX_SEGS: u16 = 39;
/// Maximum GSO segment size
pub const IFLA_GSO_MAX_SIZE: u16 = 40;

// ── IFLA_INFO constants (nested inside IFLA_LINKINFO) ──
/// Link kind (e.g. "veth", "bridge") — NUL-terminated string
pub const IFLA_INFO_KIND: u16 = 1;
/// Link kind-specific data — nested
pub const IFLA_INFO_DATA: u16 = 2;
/// Link kind-specific statistics — nested
pub const IFLA_INFO_XSTATS: u16 = 3;

// ── VETH-specific constants (nested inside IFLA_INFO_DATA) ──
/// Peer info: contains ifinfomsg + IFLA_IFNAME attributes
pub const VETH_INFO_PEER: u16 = 1;

// ── IFA constants (linux/if_addr.h) ──
/// Unspecified (padding)
pub const IFA_UNSPEC: u16 = 0;
/// Interface address (primary address)
pub const IFA_ADDRESS: u16 = 1;
/// Local address (for point-to-point links)
pub const IFA_LOCAL: u16 = 2;
/// Address label (e.g. "eth0:0")
pub const IFA_LABEL: u16 = 3;
/// Broadcast address
pub const IFA_BROADCAST: u16 = 4;
/// Anycast address
pub const IFA_ANYCAST: u16 = 5;
/// Address cache info (ifa_cacheinfo struct)
pub const IFA_CACHEINFO: u16 = 6;
/// Multicast address
pub const IFA_MULTICAST: u16 = 7;
/// Address flags (IFA_F_* bitmask)
pub const IFA_FLAGS: u16 = 8;

// ── RTA constants (linux/rtnetlink.h, route attributes) ──
/// Destination address
pub const RTA_DST: u16 = 1;
/// Output interface index
pub const RTA_OIF: u16 = 4;
/// Gateway address
pub const RTA_GATEWAY: u16 = 5;

// ── ARPHRD constants (linux/if_arp.h) ──
/// Ethernet (10/100/1000 Mb)
pub const ARPHRD_ETHER: u16 = 1;
/// Loopback device
pub const ARPHRD_LOOPBACK: u16 = 772;

// ── RTMGRP constants (linux/rtnetlink.h, netlink multicast groups) ──
/// Link state changes (RTM_NEWLINK / RTM_DELLINK)
pub const RTMGRP_LINK: u32 = 1;
/// IPv4 interface address changes (RTM_NEWADDR / RTM_DELADDR)
pub const RTMGRP_IPV4_IFADDR: u32 = 0x10;
/// IPv4 route changes (RTM_NEWROUTE / RTM_DELROUTE)
pub const RTMGRP_IPV4_ROUTE: u32 = 0x40;

// ── Helper: construct an RTA attribute ──
pub fn rta_data(rta_type: u16, payload: &[u8]) -> Vec<u8> {
    let total = 4 + nlmsg_align(payload.len());
    let mut buf = vec![0u8; total];
    buf[0..2].copy_from_slice(&((payload.len() as u16 + 4).to_ne_bytes()));
    buf[2..4].copy_from_slice(&rta_type.to_ne_bytes());
    buf[4..4 + payload.len()].copy_from_slice(payload);
    buf
}

fn pu32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_ne_bytes());
}
fn pu16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_ne_bytes());
}
fn pu8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}

// ── Helper: build a netlink message header + payload ──
pub fn build_nlmsg(msg_type: u16, flags: u16, seq: u32, pid: u32, payload: &[u8]) -> Vec<u8> {
    let total = 16 + nlmsg_align(payload.len());
    let mut buf = Vec::with_capacity(total);
    pu32(&mut buf, total as u32);
    pu16(&mut buf, msg_type);
    pu16(&mut buf, flags);
    pu32(&mut buf, seq);
    pu32(&mut buf, pid);
    buf.extend_from_slice(payload);
    while buf.len() % NLMSG_ALIGNTO != 0 {
        buf.push(0);
    }
    buf
}

/// Build a netlink error message (NLMSG_ERROR payload).
/// `orig` is the 16-byte header of the failed request.
pub fn build_nlmsg_error(errno: i32, seq: u32, pid: u32, orig: &[u8; 16]) -> Vec<u8> {
    let mut payload = Vec::new();
    pu32(&mut payload, (-errno) as u32);
    payload.extend_from_slice(orig);
    build_nlmsg(NLMSG_ERROR, 0, seq, pid, &payload)
}
