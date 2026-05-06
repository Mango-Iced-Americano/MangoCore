use super::PortManager;
use crate::mm::translated_refmut;
use crate::net::AF_INET;
use crate::net::AF_INET6;
use crate::task::current_task;
use crate::utils::error::GeneralRet;
use crate::utils::error::SyscallErr;
use crate::utils::error::SyscallRet;
use core::convert::TryInto;
use core::mem;
use core::slice;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Ipv6Address};

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
#[repr(C)]
pub struct SocketAddrv4 {
    sin_port: [u8; 2],
    sin_addr: [u8; 4],
    sin_zero: [u8; 8], //padding
}

impl SocketAddrv4 {
    /// user check first
    pub fn new(buf: &[u8]) -> Self {
        let addr = Self {
            sin_port: buf[2..4].try_into().expect("ipv4 port len err"),
            sin_addr: buf[4..8].try_into().expect("ipv4 addr len err"),
            sin_zero: [0u8; 8],
        };
        log::info!("[SocketAddrv4::new] new addr: {:?}", addr);
        addr
    }
    pub fn fill(&self, addr_buf: &mut [u8]) {
        self._fill(addr_buf);
    }
    pub fn _fill(&self, addr_buf: &mut [u8]) {
        addr_buf.fill(0);
        addr_buf[0..2].copy_from_slice(u16::to_ne_bytes(AF_INET).as_slice());
        addr_buf[2..4].copy_from_slice(self.sin_port.as_slice());
        addr_buf[4..8].copy_from_slice(self.sin_addr.as_slice());
        addr_buf[8..16].copy_from_slice(self.sin_zero.as_slice());
    }
}

impl From<IpEndpoint> for SocketAddrv4 {
    fn from(value: IpEndpoint) -> Self {
        Self {
            sin_port: value.port.to_be_bytes(),
            sin_addr: value
                .addr
                .as_bytes()
                .try_into()
                .expect("ipv4 addr len error"),
            sin_zero: [0u8; 8],
        }
    }
}

impl From<SocketAddrv4> for IpEndpoint {
    fn from(value: SocketAddrv4) -> Self {
        // big end
        let port = u16::from_be_bytes(value.sin_port);
        Self::new(IpAddress::Ipv4(Ipv4Address(value.sin_addr)), port)
    }
}

impl From<SocketAddrv4> for IpListenEndpoint {
    fn from(value: SocketAddrv4) -> Self {
        // big end
        let port = u16::from_be_bytes(value.sin_port);
        let addr = Ipv4Address(value.sin_addr);
        if addr.is_unspecified() {
            if port != 0 {
                IpListenEndpoint { addr: None, port }
            } else {
                IpListenEndpoint {
                    addr: None,
                    port: PortManager::alloc_ephemeral_port(),
                }
            }
        } else {
            IpListenEndpoint {
                addr: Some(IpAddress::Ipv4(addr)),
                port,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
#[repr(C)]
pub struct SocketAddrv6 {
    sin6_port: [u8; 2],
    sin6_flowinfo: [u8; 4],
    sin6_addr: [u8; 16],
}

impl SocketAddrv6 {
    /// user check first
    pub fn new(buf: &[u8]) -> Self {
        log::debug!("[SocketAddrv6::new] buf: {:?}", buf);
        let addr = Self {
            sin6_port: buf[2..4].try_into().expect("ipv6 port len err"),
            sin6_flowinfo: buf[4..8].try_into().expect("ipv6 flowinfo len err"),
            sin6_addr: buf[8..24].try_into().expect("ipv6 addr len err"),
        };
        log::debug!("[SocketAddrv6::new] new addr: {:?}", addr);
        addr
    }
    pub fn fill(&self, addr_buf: &mut [u8]) {
        self._fill(addr_buf);
    }
    pub fn _fill(&self, addr_buf: &mut [u8]) {
        addr_buf.fill(0);
        addr_buf[0..2].copy_from_slice(u16::to_ne_bytes(AF_INET6).as_slice());
        addr_buf[2..4].copy_from_slice(self.sin6_port.as_slice());
        addr_buf[4..8].copy_from_slice(self.sin6_flowinfo.as_slice());
        addr_buf[8..24].copy_from_slice(self.sin6_addr.as_slice());
    }
}

impl From<IpEndpoint> for SocketAddrv6 {
    fn from(value: IpEndpoint) -> Self {
        Self {
            sin6_port: value.port.to_be_bytes(),
            sin6_flowinfo: [0 as u8; 4],
            sin6_addr: value
                .addr
                .as_bytes()
                .try_into()
                .expect("ipv6 addr len error"),
        }
    }
}

impl From<SocketAddrv6> for IpEndpoint {
    fn from(value: SocketAddrv6) -> Self {
        // big end
        let port = u16::from_be_bytes(value.sin6_port);
        Self::new(IpAddress::Ipv6(Ipv6Address(value.sin6_addr)), port)
    }
}

impl From<SocketAddrv6> for IpListenEndpoint {
    fn from(value: SocketAddrv6) -> Self {
        // big end
        let port = u16::from_be_bytes(value.sin6_port);
        let addr = Ipv6Address(value.sin6_addr);
        if addr.is_unspecified() {
            if port != 0 {
                IpListenEndpoint { addr: None, port }
            } else {
                IpListenEndpoint {
                    addr: None,
                    port: PortManager::alloc_ephemeral_port(),
                }
            }
        } else {
            IpListenEndpoint {
                addr: Some(IpAddress::Ipv6(addr)),
                port,
            }
        }
    }
}
pub fn to_endpoint(listen_endpoint: IpListenEndpoint) -> IpEndpoint {
    _to_endpoint(listen_endpoint)
}

#[allow(unused)]
pub fn endpoint(addr_buf: &[u8]) -> GeneralRet<IpEndpoint> {
    _endpoint(addr_buf)
}

pub fn _to_endpoint(listen_endpoint: IpListenEndpoint) -> IpEndpoint {
    let addr = match listen_endpoint.addr {
        Some(addr) if addr.is_unspecified() => IpAddress::v4(127, 0, 0, 1),
        Some(addr) => addr,
        None => IpAddress::v4(127, 0, 0, 1),
    };
    IpEndpoint::new(addr, listen_endpoint.port)
}

#[allow(unused)]
pub fn _endpoint(addr_buf: &[u8]) -> GeneralRet<IpEndpoint> {
    let listen_endpoint = listen_endpoint(addr_buf)?;
    let addr = match listen_endpoint.addr {
        Some(addr) if addr.is_unspecified() => IpAddress::v4(127, 0, 0, 1),
        Some(addr) => addr,
        None => IpAddress::v4(127, 0, 0, 1),
    };
    Ok(IpEndpoint::new(addr, listen_endpoint.port))
}

pub fn fill_with_endpoint(endpoint: IpEndpoint, addr: usize, addrlen: usize) -> SyscallRet {
    _fill_with_endpoint(endpoint, addr, addrlen)
}
pub fn listen_endpoint(addr_buf: &[u8]) -> GeneralRet<IpListenEndpoint> {
    _listen_endpoint(addr_buf)
}
/// 将 IpListenEndpoint 转为 IpEndpoint，**保留未指定地址**（用于 getsockname）
pub fn listen_to_ip_endpoint_preserve(listen: IpListenEndpoint) -> IpEndpoint {
    let addr = match listen.addr {
        Some(addr) => addr,
        None => IpAddress::Ipv4(smoltcp::wire::Ipv4Address::UNSPECIFIED),
    };
    IpEndpoint::new(addr, listen.port)
}
pub fn _fill_with_endpoint(endpoint: IpEndpoint, addr: usize, addrlen: usize) -> SyscallRet {
    log::debug!(
        "[address::fill_with_endpoint] fill addr {} with endpoint {:?}",
        addr,
        endpoint
    );
    // NULL 指针检查：addr == 0 或 addrlen == 0 时直接返回 EFAULT
    if addr == 0 || addrlen == 0 {
        return Err(SyscallErr::EFAULT);
    }
    // 对齐检查：addrlen 指针必须 4 字节对齐（RISC-V 未对齐访问可能静默成功）
    if addrlen % 4 != 0 {
        return Err(SyscallErr::EFAULT);
    }
    let task = current_task().unwrap();
    let token = task.get_user_token();
    let addr = match translated_refmut(token, addr as *mut u8) {
        Ok(p) => p,
        Err(_) => return Err(SyscallErr::EFAULT),
    };
    let addrlen = match translated_refmut(token, addrlen as *mut u32) {
        Ok(p) => p,
        Err(_) => return Err(SyscallErr::EFAULT),
    };
    // 校验 addrlen 至少能容纳 sa_family 字段（2 字节）
    if *addrlen < 2 {
        return Err(SyscallErr::EINVAL);
    }
    // socklen_t 在 Linux 上是 signed int，负值表示无效 → EINVAL
    if (*addrlen as i32) < 0 {
        return Err(SyscallErr::EINVAL);
    }
    // 校验 *addrlen 是否足够容纳 sockaddr 结构
    let required = match endpoint.addr {
        IpAddress::Ipv4(_) => 16,
        IpAddress::Ipv6(_) => 24,
    };
    if *addrlen < required {
        return Err(SyscallErr::EINVAL);
    }
    // let mut buf = [0u8; 24]; // ipv6最大24字节
    match endpoint.addr {
        IpAddress::Ipv4(_) => {
            let len = mem::size_of::<u16>() + mem::size_of::<SocketAddrv4>();
            let addr_buf = unsafe { slice::from_raw_parts_mut(addr as *mut u8, len) };
            SocketAddrv4::from(endpoint).fill(addr_buf);
            *addrlen = 16;
        }
        IpAddress::Ipv6(_) => {
            let len = mem::size_of::<u16>() + mem::size_of::<SocketAddrv6>();
            let addr_buf = unsafe { slice::from_raw_parts_mut(addr as *mut u8, len) };
            SocketAddrv6::from(endpoint).fill(addr_buf);
            *addrlen = 24;
        }
    }
    Ok(0)
}

pub fn _listen_endpoint(addr_buf: &[u8]) -> GeneralRet<IpListenEndpoint> {
    if addr_buf.len() < 2 {
        return Err(SyscallErr::EINVAL);
    }
    let family = u16::from_ne_bytes(addr_buf[0..2].try_into().map_err(|_| SyscallErr::EINVAL)?);
    log::info!("[address::listen_enpoint] addr family {}", family);
    match family {
        AF_INET => {
            if addr_buf.len() < 8 {
                // 2(family) + 2(port) + 4(addr) = 8
                return Err(SyscallErr::EINVAL);
            }
            let ipv4 = SocketAddrv4::new(addr_buf);
            Ok(IpListenEndpoint::from(ipv4))
        }
        AF_INET6 => {
            if addr_buf.len() < 24 {
                // 2(family) + 2(port) + 16(addr) = 24
                return Err(SyscallErr::EINVAL);
            }
            let ipv6 = SocketAddrv6::new(addr_buf);
            Ok(IpListenEndpoint::from(ipv6))
        }
        _ => return Err(SyscallErr::EAFNOSUPPORT),
    }
}

/// 从用户空间 sockaddr 缓冲区解析出 IpEndpoint。
/// ptr 必须指向至少 len 字节的可读内存。
pub fn read_sockaddr(ptr: *const u8, len: u32) -> GeneralRet<IpEndpoint> {
    if len < 2 {
        return Err(SyscallErr::EINVAL);
    }
    let buf = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
    match listen_endpoint(buf) {
        Ok(listen_endpoint) => Ok(to_endpoint(listen_endpoint)),
        Err(e) => Err(e),
    }
}

/// 将端点信息写回用户空间 sockaddr 缓冲区，并更新 addrlen。
pub fn write_sockaddr(endpoint: IpEndpoint, addr: *mut u8, addrlen: *mut u32) -> SyscallRet {
    // 直接复用已有的 fill_with_endpoint
    // 但它接受 usize 参数，我们需要转换
    _fill_with_endpoint(endpoint, addr as usize, addrlen as usize)
}
