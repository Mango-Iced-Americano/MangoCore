use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::net::{Endpoint, PSOCK, Socket};
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};

pub mod netlink;
pub mod route;
pub mod segment;

/// Maximum number of netlink messages in a socket's recv_queue.
const MAX_NETLINK_QUEUE_LEN: usize = 1024;
/// Maximum total bytes in a socket's recv_queue (256KB).
const MAX_NETLINK_QUEUE_BYTES: usize = 256 * 1024;

pub struct NetlinkSocket {
    pub protocol: u32,
    pub recv_queue: spin::Mutex<VecDeque<Vec<u8>>>,
}

impl NetlinkSocket {
    pub fn new(protocol: u32) -> Self {
        Self { protocol, recv_queue: spin::Mutex::new(VecDeque::new()) }
    }

    /// Push a message to the recv_queue with bounds checking.
    /// Returns `true` if the message was pushed, `false` if the queue is full
    /// (either by message count or total bytes).
    pub fn push_recv(&self, msg: Vec<u8>) -> bool {
        let mut q = self.recv_queue.lock();
        if q.len() >= MAX_NETLINK_QUEUE_LEN {
            return false;
        }
        let total_bytes: usize = q.iter().map(|m| m.len()).sum();
        if total_bytes + msg.len() > MAX_NETLINK_QUEUE_BYTES {
            return false;
        }
        q.push_back(msg);
        true
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
    fn local_endpoint(&self) -> Option<Endpoint> { Some(Endpoint::Netlink(0)) }
    fn remote_endpoint(&self) -> Option<Endpoint> { None }
    fn shutdown(&self, _h: u32) -> GeneralRet<()> { Ok(()) }
    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut q = self.recv_queue.lock();
        if let Some(data) = q.pop_front() {
            let len = data.len().min(buf.len()); buf[..len].copy_from_slice(&data[..len]);
            Ok(len as isize)
        } else { Err(SyscallErr::EAGAIN) }
    }
    fn try_send(&self, buf: &[u8], _f: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        self.try_sendmsg(buf, None, _f)
    }
    fn try_sendmsg(&self, buf: &[u8], _dest: Option<Endpoint>, _flags: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        // Process all nlmsghdrs in the buffer.  Reports the full buffer length
        // so that userspace write() knows all bytes were consumed — returning 0
        // would cause BusyBox's full_write to loop and fill the recv_queue with
        // duplicate ACKs until ENOBUFS.
        let mut consumed = 0usize;
        while consumed + 16 <= buf.len() {
            let nlmsg_len = u32::from_ne_bytes([
                buf[consumed],
                buf[consumed + 1],
                buf[consumed + 2],
                buf[consumed + 3],
            ]) as usize;
            if nlmsg_len < 16 || consumed + nlmsg_len > buf.len() {
                break;
            }
            route::handle_netlink_msg(&buf[consumed..consumed + nlmsg_len], self)?;
            // Advance to next nlmsghdr, aligned to 4-byte boundary.
            consumed += crate::net::socket::netlink::netlink::nlmsg_align(nlmsg_len);
        }
        if consumed == 0 {
            return Err(crate::utils::error::SyscallErr::EINVAL);
        }
        Ok(buf.len() as isize)
    }
    fn socket_r_ready(&self) -> bool { !self.recv_queue.lock().is_empty() }
}
