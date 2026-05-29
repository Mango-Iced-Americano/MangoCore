use alloc::vec;
use alloc::vec::Vec;

pub const NLMSG_ALIGNTO: usize = 4;
pub const NLM_F_REQUEST: u16 = 0x01;
pub const NLM_F_MULTI: u16 = 0x02;
pub const NLM_F_ROOT: u16 = 0x100;
pub const NLM_F_MATCH: u16 = 0x200;
pub const NLM_F_DUMP: u16 = NLM_F_ROOT | NLM_F_MATCH;
pub const NLMSG_ERROR: u16 = 2;
pub const NLMSG_DONE: u16 = 3;
pub const RTM_NEWLINK: u16 = 16;
pub const RTM_GETLINK: u16 = 18;
pub const RTM_NEWADDR: u16 = 20;
pub const RTM_GETADDR: u16 = 22;
pub const RTM_NEWROUTE: u16 = 24;
pub const RTM_GETROUTE: u16 = 26;
pub const IFLA_IFNAME: u16 = 3;
pub const IFLA_MTU: u16 = 4;
pub const IFLA_ADDRESS: u16 = 1;
pub const ARPHRD_LOOPBACK: u16 = 772;
pub const ARPHRD_ETHER: u16 = 1;
pub const IFA_ADDRESS: u16 = 1;
pub const IFA_LOCAL: u16 = 2;
pub const IFA_LABEL: u16 = 3;
pub const RTA_DST: u16 = 1;
pub const RTA_GATEWAY: u16 = 5;
pub const RTA_OIF: u16 = 4;

pub fn nlmsg_align(len: usize) -> usize { (len + NLMSG_ALIGNTO - 1) & !(NLMSG_ALIGNTO - 1) }

pub fn rta_data(rta_type: u16, payload: &[u8]) -> Vec<u8> {
    let total = 4 + nlmsg_align(payload.len());
    let mut buf = vec![0u8; total];
    buf[0..2].copy_from_slice(&((payload.len() as u16 + 4).to_ne_bytes()));
    buf[2..4].copy_from_slice(&rta_type.to_ne_bytes());
    buf[4..4 + payload.len()].copy_from_slice(payload);
    buf
}

fn pu32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_ne_bytes()); }
fn pu16(buf: &mut Vec<u8>, v: u16) { buf.extend_from_slice(&v.to_ne_bytes()); }
fn pu8(buf: &mut Vec<u8>, v: u8) { buf.push(v); }

pub fn build_nlmsg(msg_type: u16, flags: u16, seq: u32, pid: u32, payload: &[u8]) -> Vec<u8> {
    let total = 16 + nlmsg_align(payload.len());
    let mut buf = Vec::with_capacity(total);
    pu32(&mut buf, total as u32);
    pu16(&mut buf, msg_type);
    pu16(&mut buf, flags);
    pu32(&mut buf, seq);
    pu32(&mut buf, pid);
    buf.extend_from_slice(payload);
    while buf.len() % NLMSG_ALIGNTO != 0 { buf.push(0); }
    buf
}

pub fn build_nlmsg_error(errno: i32, seq: u32, pid: u32, orig: &[u8; 16]) -> Vec<u8> {
    let mut payload = Vec::new();
    pu32(&mut payload, (-errno) as u32);
    payload.extend_from_slice(orig);
    build_nlmsg(NLMSG_ERROR, 0, seq, pid, &payload)
}
