use crate::mm::copy_from_user_array;
use crate::task::current_user_token;
use crate::utils::error::SyscallErr;
use alloc::vec::Vec;

/// Maximum address length for sockaddr — normally very small, 512 is extremely generous
pub const MAX_ADDR_LEN: usize = 512;

/// level
pub const SOL_SOCKET: u32 = 1;
pub const SOL_TCP: u32 = 6;
pub const SOL_IP: u32 = 0;
pub const SOL_IPV6: u32 = 41;
pub const IPV6_V6ONLY: u32 = 26;
pub const SOL_ICMPV6: u32 = 58;
pub const IP_HDRINCL: u32 = 3;
pub const IPV6_RECVPKTINFO: u32 = 49;
pub const IPV6_RECVHOPLIMIT: u32 = 53;
pub const ICMP6_FILTER: u32 = 1;

pub const SOL_RAW: u32 = 255;
pub const IPV6_CHECKSUM: u32 = 7;

/// Returns true if `level` is a commonly known socket option protocol level.
/// Used to distinguish between ENOPROTOOPT (known level, unknown option)
/// and EOPNOTSUPP (unknown level).
pub fn is_known_sockopt_level(level: u32) -> bool {
    matches!(
        level,
        SOL_IP | SOL_SOCKET | SOL_TCP | SOL_IPV6 | SOL_ICMPV6 | 17 /* SOL_UDP */ | 255 /* SOL_RAW */
    )
}

/// option name (TCP)
pub const TCP_NODELAY: u32 = 1;
pub const TCP_MAXSEG: u32 = 2;
#[allow(unused)]
pub const TCP_INFO: u32 = 11;
pub const TCP_CONGESTION: u32 = 13;

/// option name (IP)
pub const IP_RECVERR: u32 = 11;
pub const MCAST_JOIN_GROUP: u32 = 42;
pub const MCAST_LEAVE_GROUP: u32 = 45;

/// option name (socket)
pub const SO_DEBUG: u32 = 1;
pub const SO_REUSEADDR: u32 = 2;
pub const SO_PEERCRED: u32 = 17;
pub const SO_TYPE: u32 = 3;
pub const SO_ERROR: u32 = 4;
pub const SO_DONTROUTE: u32 = 5;
pub const SO_BROADCAST: u32 = 6;
pub const SO_SNDBUF: u32 = 7;
pub const SO_RCVBUF: u32 = 8;
pub const SO_KEEPALIVE: u32 = 9;
pub const SO_OOBINLINE: u32 = 10;
pub const SO_REUSEPORT: u32 = 15;
pub const SO_BINDTODEVICE: u32 = 25;
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

/// 将当前任务的 sockaddr 复制到内核所有的连续缓冲区。
///
/// Endpoint 解析不得持有用户物理页 slice；512 字节上限也避免把不可信长度直接用于大分配。
pub fn read_sockaddr(addr: usize, addrlen: u32) -> Result<Vec<u8>, SyscallErr> {
    check_addrlen(addrlen)?;
    let len = addrlen as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    if addr == 0 {
        return Err(SyscallErr::EFAULT);
    }

    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| SyscallErr::ENOMEM)?;
    bytes.resize(len, 0);
    copy_from_user_array(
        current_user_token(),
        addr as *const u8,
        bytes.as_mut_ptr(),
        len,
    )
    .map_err(|_| SyscallErr::EFAULT)?;
    Ok(bytes)
}
