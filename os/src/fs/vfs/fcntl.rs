//! fcntl command types and POSIX file lock structures.
//!
//! Separated from `file.rs` to keep the File struct focused on VFS operations.

use num_enum::TryFromPrimitive;

#[derive(Debug, Copy, Clone, Eq, PartialEq, TryFromPrimitive)]
#[repr(u32)]
pub enum FcntlCommand {
    DupFd = 0,
    GetFd = 1,
    SetFd = 2,
    GetFlags = 3,
    SetFlags = 4,
    GetLock = 5,
    SetLock = 6,
    SetLockWait = 7,
    SetOwn = 8,
    GetOwn = 9,
    SetSig = 10,
    GetSig = 11,
    SetOwnEx = 15,
    GetOwnEx = 16,
    GetOwnerUids = 17,
    OfdGetLock = 36,
    OfdSetLock = 37,
    OfdSetLockWait = 38,
    SetLease = 1024,
    GetLease = 1025,
    Notify = 1026,
    CreatedQuery = 1028,
    CancelLock = 1029,
    DupFdCloexec = 1030,
    SetPipeSize = 1031,
    GetPipeSize = 1032,
    AddSeals = 1033,
    GetSeals = 1034,
    GetRwHint = 1035,
    SetRwHint = 1036,
    GetFileRwHint = 1037,
    SetFileRwHint = 1038,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PosixFlock {
    pub l_type: i16,
    pub l_whence: i16,
    pub l_start: i64,
    pub l_len: i64,
    pub l_pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FOwnerEx {
    pub type_: i32,
    pub pid: i32,
}

pub const FD_CLOEXEC: usize = 1;
pub const F_RDLCK: i16 = 0;
pub const F_WRLCK: i16 = 1;
pub const F_UNLCK: i16 = 2;
pub const F_OWNER_TID: i32 = 0;
pub const F_OWNER_PID: i32 = 1;
pub const F_OWNER_PGRP: i32 = 2;
pub const F_SEAL_SEAL: usize = 0x0001;
pub const F_SEAL_SHRINK: usize = 0x0002;
pub const F_SEAL_GROW: usize = 0x0004;
pub const F_SEAL_WRITE: usize = 0x0008;
pub const F_SEAL_FUTURE_WRITE: usize = 0x0010;
