use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::net::{Endpoint, PSOCK, Socket};
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};

pub mod netlink;
pub mod route;

pub struct NetlinkSocket {
    pub protocol: u32,
    pub recv_queue: spin::Mutex<VecDeque<Vec<u8>>>,
}

impl NetlinkSocket {
    pub fn new(protocol: u32) -> Self {
        Self { protocol, recv_queue: spin::Mutex::new(VecDeque::new()) }
    }
}

impl Socket for NetlinkSocket {
    fn bind(&self, _ep: &Endpoint) -> SyscallRet { Ok(0) }
    fn listen(&self) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn connect(&self, _ep: &Endpoint) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn accept(&self, _fd: u32, _a: usize, _l: usize) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn socket_type(&self) -> PSOCK { PSOCK::Raw }
    fn recv_buf_size(&self) -> usize { 65536 }
    fn send_buf_size(&self) -> usize { 65536 }
    fn set_recv_buf_size(&self, _s: usize) {}
    fn set_send_buf_size(&self, _s: usize) {}
    fn local_endpoint(&self) -> Option<Endpoint> { Some(Endpoint::Unspecified) }
    fn remote_endpoint(&self) -> Option<Endpoint> { None }
    fn shutdown(&self, _h: u32) -> GeneralRet<()> { Ok(()) }
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut q = self.recv_queue.lock();
        if let Some(data) = q.pop_front() {
            let len = data.len().min(buf.len()); buf[..len].copy_from_slice(&data[..len]);
            Ok(len as isize)
        } else { Err(SyscallErr::EAGAIN) }
    }
    fn try_send(&self, _buf: &[u8], _f: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        Err(SyscallErr::EOPNOTSUPP)
    }
    fn try_sendmsg(&self, buf: &[u8], _dest: Option<Endpoint>, _flags: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        route::handle_netlink_msg(buf, self)
    }
    fn socket_r_ready(&self) -> bool { !self.recv_queue.lock().is_empty() }
}
