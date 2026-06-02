//! Netlink route segment message types.
//! Provides generic [`SegmentCommon`] with [`CMsgSegHdr`] and [`CAttrHeader`],
//! concrete body types for link/addr/route messages, and the [`RouteNlSegment`] enum.
//! Design follows Linux uapi `<linux/netlink.h>` / `<linux/rtnetlink.h>`.

use alloc::vec::Vec;
use core::mem::size_of;

/// Netlink attribute nesting flag.
/// When matching `rta_type`, apply `& !NLA_F_NESTED` to strip this bit.
pub const NLA_F_NESTED: u16 = 0x8000;

/// 16-byte netlink message header (Linux `struct nlmsghdr`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CMsgSegHdr {
    pub len: u32,
    pub type_: u16,
    pub flags: u16,
    pub seq: u32,
    pub pid: u32,
}

/// 4-byte netlink attribute header (Linux `struct rtattr`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CAttrHeader {
    pub len: u16,
    pub type_: u16,
}

/// Trait for a netlink message body that can be parsed from / serialized to bytes.
pub trait SegmentBody: Sized {
    fn parse(data: &[u8]) -> Result<Self, isize>;
    fn body_len() -> usize;
    fn to_body_bytes(&self) -> Vec<u8>;
}

/// Trait for a netlink attribute type that supports batch parse / serialize.
pub trait SegmentAttr: Sized {
    fn parse_many(data: &[u8]) -> Result<Vec<Self>, isize>;
    fn to_attrs_bytes(attrs: &[Self]) -> Vec<u8>;
}

/// A parsed netlink segment with a generic body and a vector of attributes.
#[derive(Debug, Clone)]
pub struct SegmentCommon<Body, Attr> {
    pub header: CMsgSegHdr,
    pub body: Body,
    pub attrs: Vec<Attr>,
}

impl<Body: SegmentBody, Attr: SegmentAttr> SegmentCommon<Body, Attr> {
    pub fn read_from_buf(buf: &[u8]) -> Result<Self, isize> {
        if buf.len() < size_of::<CMsgSegHdr>() {
            return Err(-22);
        }
        let header = CMsgSegHdr {
            len: u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]),
            type_: u16::from_ne_bytes([buf[4], buf[5]]),
            flags: u16::from_ne_bytes([buf[6], buf[7]]),
            seq: u32::from_ne_bytes([buf[8], buf[9], buf[10], buf[11]]),
            pid: u32::from_ne_bytes([buf[12], buf[13], buf[14], buf[15]]),
        };

        let body_start = size_of::<CMsgSegHdr>();
        let body_end = body_start + Body::body_len();
        if buf.len() < body_end {
            return Err(-22);
        }
        let body = Body::parse(&buf[body_start..body_end])?;
        let attrs = Attr::parse_many(&buf[body_end..])?;

        Ok(Self { header, body, attrs })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let body_bytes = self.body.to_body_bytes();
        let body_aligned = nlmsg_align(body_bytes.len());
        let attrs_bytes = Attr::to_attrs_bytes(&self.attrs);

        let hdr_size = size_of::<CMsgSegHdr>();
        let total = hdr_size + body_aligned + attrs_bytes.len();

        let mut buf = Vec::with_capacity(total);

        buf.extend_from_slice(&(total as u32).to_ne_bytes());
        buf.extend_from_slice(&self.header.type_.to_ne_bytes());
        buf.extend_from_slice(&self.header.flags.to_ne_bytes());
        buf.extend_from_slice(&self.header.seq.to_ne_bytes());
        buf.extend_from_slice(&self.header.pid.to_ne_bytes());

        buf.extend_from_slice(&body_bytes);
        while buf.len() < hdr_size + body_aligned {
            buf.push(0);
        }

        buf.extend_from_slice(&attrs_bytes);

        buf
    }
}

/// Link info message body (Linux `struct ifinfomsg`, 16 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CIfinfoMsg {
    pub family: u8,
    pub pad: u8,
    pub type_: u16,
    pub index: i32,
    pub flags: u32,
    pub change: u32,
}

impl SegmentBody for CIfinfoMsg {
    fn body_len() -> usize {
        size_of::<Self>()
    }

    fn parse(data: &[u8]) -> Result<Self, isize> {
        if data.len() < size_of::<Self>() {
            return Err(-22);
        }
        Ok(Self {
            family: data[0],
            pad: data[1],
            type_: u16::from_ne_bytes([data[2], data[3]]),
            index: i32::from_ne_bytes([data[4], data[5], data[6], data[7]]),
            flags: u32::from_ne_bytes([data[8], data[9], data[10], data[11]]),
            change: u32::from_ne_bytes([data[12], data[13], data[14], data[15]]),
        })
    }

    fn to_body_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(size_of::<Self>());
        buf.push(self.family);
        buf.push(self.pad);
        buf.extend_from_slice(&self.type_.to_ne_bytes());
        buf.extend_from_slice(&self.index.to_ne_bytes());
        buf.extend_from_slice(&self.flags.to_ne_bytes());
        buf.extend_from_slice(&self.change.to_ne_bytes());
        buf
    }
}

/// Address info message body (Linux `struct ifaddrmsg`, 8 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CIfaddrMsg {
    pub family: u8,
    pub prefixlen: u8,
    pub flags: u8,
    pub scope: u8,
    pub index: i32,
}

impl SegmentBody for CIfaddrMsg {
    fn body_len() -> usize {
        size_of::<Self>()
    }

    fn parse(data: &[u8]) -> Result<Self, isize> {
        if data.len() < size_of::<Self>() {
            return Err(-22);
        }
        Ok(Self {
            family: data[0],
            prefixlen: data[1],
            flags: data[2],
            scope: data[3],
            index: i32::from_ne_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    fn to_body_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(size_of::<Self>());
        buf.push(self.family);
        buf.push(self.prefixlen);
        buf.push(self.flags);
        buf.push(self.scope);
        buf.extend_from_slice(&self.index.to_ne_bytes());
        buf
    }
}

/// Route message body (Linux `struct rtmsg`, 12 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CRtMsg {
    pub family: u8,
    pub dst_len: u8,
    pub src_len: u8,
    pub tos: u8,
    pub table: u8,
    pub protocol: u8,
    pub scope: u8,
    pub type_: u8,
    pub flags: u32,
}

impl SegmentBody for CRtMsg {
    fn body_len() -> usize {
        size_of::<Self>()
    }

    fn parse(data: &[u8]) -> Result<Self, isize> {
        if data.len() < size_of::<Self>() {
            return Err(-22);
        }
        Ok(Self {
            family: data[0],
            dst_len: data[1],
            src_len: data[2],
            tos: data[3],
            table: data[4],
            protocol: data[5],
            scope: data[6],
            type_: data[7],
            flags: u32::from_ne_bytes([data[8], data[9], data[10], data[11]]),
        })
    }

    fn to_body_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(size_of::<Self>());
        buf.push(self.family);
        buf.push(self.dst_len);
        buf.push(self.src_len);
        buf.push(self.tos);
        buf.push(self.table);
        buf.push(self.protocol);
        buf.push(self.scope);
        buf.push(self.type_);
        buf.extend_from_slice(&self.flags.to_ne_bytes());
        buf
    }
}

/// Error body (NLMSG_ERROR): error code + original request header.
#[derive(Debug, Clone)]
pub struct ErrorSegmentBody {
    pub error_code: i32,
    pub request_header: CMsgSegHdr,
}

impl SegmentBody for ErrorSegmentBody {
    fn body_len() -> usize {
        4 + size_of::<CMsgSegHdr>()
    }

    fn parse(data: &[u8]) -> Result<Self, isize> {
        let body_len = 4 + size_of::<CMsgSegHdr>();
        if data.len() < body_len {
            return Err(-22);
        }
        Ok(Self {
            error_code: i32::from_ne_bytes([data[0], data[1], data[2], data[3]]),
            request_header: CMsgSegHdr {
                len: u32::from_ne_bytes([data[4], data[5], data[6], data[7]]),
                type_: u16::from_ne_bytes([data[8], data[9]]),
                flags: u16::from_ne_bytes([data[10], data[11]]),
                seq: u32::from_ne_bytes([data[12], data[13], data[14], data[15]]),
                pid: u32::from_ne_bytes([data[16], data[17], data[18], data[19]]),
            },
        })
    }

    fn to_body_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + size_of::<CMsgSegHdr>());
        buf.extend_from_slice(&self.error_code.to_ne_bytes());
        buf.extend_from_slice(&self.request_header.len.to_ne_bytes());
        buf.extend_from_slice(&self.request_header.type_.to_ne_bytes());
        buf.extend_from_slice(&self.request_header.flags.to_ne_bytes());
        buf.extend_from_slice(&self.request_header.seq.to_ne_bytes());
        buf.extend_from_slice(&self.request_header.pid.to_ne_bytes());
        buf
    }
}

/// Done body (NLMSG_DONE): end-of-multipart marker with error code.
#[derive(Debug, Clone)]
pub struct DoneSegmentBody {
    pub error_code: i32,
}

impl SegmentBody for DoneSegmentBody {
    fn body_len() -> usize {
        4
    }

    fn parse(data: &[u8]) -> Result<Self, isize> {
        if data.len() < 4 {
            return Err(-22);
        }
        Ok(Self {
            error_code: i32::from_ne_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    fn to_body_bytes(&self) -> Vec<u8> {
        self.error_code.to_ne_bytes().to_vec()
    }
}

/// Placeholder link attribute (IFLA_*) – no variants yet.
#[derive(Debug, Clone)]
pub enum LinkAttr {}

/// Placeholder address attribute (IFA_*) – no variants yet.
#[derive(Debug, Clone)]
pub enum AddrAttr {}

/// Placeholder route attribute (RTA_*) – no variants yet.
#[derive(Debug, Clone)]
pub enum RouteAttr {}

/// Zero-sized marker for segments that carry no attributes (Error / Done).
#[derive(Debug, Clone)]
pub struct NoAttr;

impl SegmentAttr for NoAttr {
    fn parse_many(_data: &[u8]) -> Result<Vec<Self>, isize> {
        Ok(Vec::new())
    }
    fn to_attrs_bytes(_attrs: &[Self]) -> Vec<u8> {
        Vec::new()
    }
}

impl SegmentAttr for LinkAttr {
    fn parse_many(_data: &[u8]) -> Result<Vec<Self>, isize> {
        Ok(Vec::new())
    }
    fn to_attrs_bytes(_attrs: &[Self]) -> Vec<u8> {
        Vec::new()
    }
}

impl SegmentAttr for AddrAttr {
    fn parse_many(_data: &[u8]) -> Result<Vec<Self>, isize> {
        Ok(Vec::new())
    }
    fn to_attrs_bytes(_attrs: &[Self]) -> Vec<u8> {
        Vec::new()
    }
}

impl SegmentAttr for RouteAttr {
    fn parse_many(_data: &[u8]) -> Result<Vec<Self>, isize> {
        Ok(Vec::new())
    }
    fn to_attrs_bytes(_attrs: &[Self]) -> Vec<u8> {
        Vec::new()
    }
}

pub type LinkSegmentBody = CIfinfoMsg;
pub type AddrSegmentBody = CIfaddrMsg;
pub type RouteSegmentBody = CRtMsg;

pub type LinkSegment = SegmentCommon<CIfinfoMsg, LinkAttr>;
pub type AddrSegment = SegmentCommon<CIfaddrMsg, AddrAttr>;
pub type RouteSegment = SegmentCommon<CRtMsg, RouteAttr>;
pub type ErrorSegment = SegmentCommon<ErrorSegmentBody, NoAttr>;
pub type DoneSegment = SegmentCommon<DoneSegmentBody, NoAttr>;

/// Top-level netlink route-family message discriminator.
#[derive(Debug, Clone)]
pub enum RouteNlSegment {
    NewLink(LinkSegment),
    DelLink(LinkSegment),
    SetLink(LinkSegment),
    GetLink(LinkSegment),
    NewAddr(AddrSegment),
    DelAddr(AddrSegment),
    GetAddr(AddrSegment),
    NewRoute(RouteSegment),
    DelRoute(RouteSegment),
    GetRoute(RouteSegment),
    Error(ErrorSegment),
    Done(DoneSegment),
}

/// Align length to the next 4-byte boundary (NLMSG_ALIGNTO = 4).
pub fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}
