use crate::utils::error::SyscallErr;

/// Maximum address length for sockaddr — normally very small, 512 is extremely generous
pub const MAX_ADDR_LEN: usize = 512;

/// level
pub const SOL_SOCKET: u32 = 1;
pub const SOL_TCP: u32 = 6;
pub const SOL_IP: u32 = 0;

/// Returns true if `level` is a commonly known socket option protocol level.
/// Used to distinguish between ENOPROTOOPT (known level, unknown option)
/// and EOPNOTSUPP (unknown level).
pub fn is_known_sockopt_level(level: u32) -> bool {
    matches!(
        level,
        SOL_IP | SOL_SOCKET | SOL_TCP | 17 /* SOL_UDP */ | 41 /* SOL_IPV6 */ | 255 /* SOL_RAW */
    )
}

/// option name (TCP)
pub const TCP_NODELAY: u32 = 1;
pub const TCP_MAXSEG: u32 = 2;
#[allow(unused)]
pub const TCP_INFO: u32 = 11;
pub const TCP_CONGESTION: u32 = 13;

/// option name (IP)
pub const MCAST_JOIN_GROUP: u32 = 42;
pub const MCAST_LEAVE_GROUP: u32 = 45;

/// option name (socket)
pub const SO_DEBUG: u32 = 1;
pub const SO_REUSEADDR: u32 = 2;
pub const SO_TYPE: u32 = 3;
pub const SO_ERROR: u32 = 4;
pub const SO_DONTROUTE: u32 = 5;
pub const SO_BROADCAST: u32 = 6;
pub const SO_SNDBUF: u32 = 7;
pub const SO_RCVBUF: u32 = 8;
pub const SO_KEEPALIVE: u32 = 9;
pub const SO_OOBINLINE: u32 = 10;
pub const SO_REUSEPORT: u32 = 15;
pub const SO_RCVTIMEO: u32 = 20;
pub const SO_SNDTIMEO: u32 = 21;

bitflags! {
    /// MSG flags for send/recv syscalls.
    pub struct MsgFlags: u32 {
        const MSG_OOB       = 0x0001;
        const MSG_PEEK      = 0x0002;
        const MSG_DONTROUTE = 0x0004;
        const MSG_CTRUNC    = 0x0008;
        const MSG_PROXY     = 0x0010;
        const MSG_TRUNC     = 0x0020;
        const MSG_DONTWAIT  = 0x0040;
        const MSG_EOR       = 0x0080;
        const MSG_WAITALL   = 0x0100;
        const MSG_FIN       = 0x0200;
        const MSG_SYN       = 0x0400;
        const MSG_CONFIRM   = 0x0800;
        const MSG_RST       = 0x1000;
        const MSG_ERRQUEUE  = 0x2000;
        const MSG_NOSIGNAL  = 0x4000;
        const MSG_MORE      = 0x8000;
    }
}

impl MsgFlags {
    /// Validate flags for recv syscalls (recvfrom, recvmsg, etc.).
    ///
    /// Returns `Ok(is_nonblock)` if flags are acceptable, or `Err(errno)`
    /// when an unsupported flag is set (e.g. `MSG_OOB`, `MSG_ERRQUEUE`).
    pub fn validate_for_recv(self) -> Result<bool, SyscallErr> {
        match () {
            _ if self.contains(MsgFlags::MSG_OOB) => Err(SyscallErr::EINVAL),
            _ if self.contains(MsgFlags::MSG_ERRQUEUE) => Err(SyscallErr::EAGAIN),
            _ => Ok(self.contains(MsgFlags::MSG_DONTWAIT)),
        }
    }

    /// Validate flags for send syscalls (sendto, sendmsg, etc.).
    pub fn validate_for_send(self) -> Result<bool, SyscallErr> {
        match () {
            _ if self.contains(MsgFlags::MSG_OOB) => Err(SyscallErr::EOPNOTSUPP),
            _ if self.contains(MsgFlags::MSG_ERRQUEUE) => Err(SyscallErr::EOPNOTSUPP),
            _ => Ok(self.contains(MsgFlags::MSG_DONTWAIT)),
        }
    }
}

pub fn check_addrlen(addrlen: u32) -> Result<(), SyscallErr> {
    if addrlen > MAX_ADDR_LEN as u32 {
        Err(SyscallErr::EINVAL)
    } else {
        Ok(())
    }
}
