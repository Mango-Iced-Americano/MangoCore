#![allow(dead_code)]

pub type PageProt = super::map_area::MapPermission;
#[allow(unused_imports)]
pub use super::page_table::{FaultAccess, UserAccess};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemAttr {
    Cached,
    Uncached,
    Device,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PageFaultKind {
    BadAddress,
    NoPermission,
    NotPresent,
    NotMapped,
    AlreadyMapped,
    LazyAlloc,
    FileBacked,
    BeyondEof,
    Cow,
    SharedWrite,
    Compressed,
    SwappedOut,
    StaleLazyPte,
    Mapped,
    Invalid,
}
