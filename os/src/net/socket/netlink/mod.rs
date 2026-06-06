use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use crate::net::{Endpoint, PSOCK, Socket};
use crate::task::WaitQueue;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
use spin::Mutex;

pub mod netlink;
pub mod route;
pub mod segment;

/// Maximum number of netlink messages in a socket's recv_queue.
const MAX_NETLINK_QUEUE_LEN: usize = 1024;
/// Maximum total bytes in a socket's recv_queue (256KB).
const MAX_NETLINK_QUEUE_BYTES: usize = 256 * 1024;

static NEXT_NETLINK_PORTID: AtomicU32 = AtomicU32::new(1);

pub struct NetlinkSocket {
    pub protocol: u32,
    pub recv_queue: spin::Mutex<VecDeque<Vec<u8>>>,
    /// WaitQueue for blocking recv — woken when push_recv() adds data.
    pub recv_wait: Mutex<WaitQueue>,
    /// Local netlink port id assigned by bind().
    /// Used in reply nlmsghdr.nlmsg_pid; BusyBox compares this against rth->local.nl_pid.
    local_portid: Mutex<u32>,
}

impl NetlinkSocket {
    pub fn new(protocol: u32) -> Self {
        Self {
            protocol,
            recv_queue: spin::Mutex::new(VecDeque::new()),
            recv_wait: Mutex::new(WaitQueue::new()),
            local_portid: Mutex::new(0),
        }
    }

    pub fn local_portid(&self) -> u32 {
        *self.local_portid.lock()
    }

    /// Push a message to the recv_queue with bounds checking.
    /// Returns `true` if the message was pushed, `false` if the queue is full
    /// (either by message count or total bytes).
    /// Wakes any blocked recv waiters on success.
    pub fn push_recv(&self, msg: Vec<u8>) -> bool {
        let msg_len = msg.len();
        let mut q = self.recv_queue.lock();
        if q.len() >= MAX_NETLINK_QUEUE_LEN {
            log::warn!("[netlink] recv_queue full (count={})", q.len());
            return false;
        }
        let total_bytes: usize = q.iter().map(|m| m.len()).sum();
        if total_bytes + msg_len > MAX_NETLINK_QUEUE_BYTES {
            log::warn!("[netlink] recv_queue full (bytes={})", total_bytes);
            return false;
        }
        q.push_back(msg);
        drop(q);
        self.recv_wait.lock().wake_all();
        log::warn!("[netlink] push_recv: {} bytes, queue_depth={}", msg_len, {
            self.recv_queue.lock().len()
        });
        true
    }
}

impl Socket for NetlinkSocket {
    fn bind(&self, ep: &Endpoint) -> SyscallRet {
        if let Endpoint::Netlink(0) = ep {
            let id = NEXT_NETLINK_PORTID.fetch_add(1, Ordering::Relaxed);
            *self.local_portid.lock() = id;
        }
        Ok(0)
    }
    fn listen(&self) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn connect(&self, _ep: &Endpoint) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn accept(&self, _fd: u32, _a: usize, _l: usize) -> SyscallRet { Err(SyscallErr::EOPNOTSUPP) }
    fn socket_type(&self) -> PSOCK { PSOCK::Raw }
    fn recv_buf_size(&self) -> usize { 65536 }
    fn send_buf_size(&self) -> usize { 65536 }
    fn set_recv_buf_size(&self, _s: usize) {}
    fn set_send_buf_size(&self, _s: usize) {}
    fn local_endpoint(&self) -> Option<Endpoint> {
        let id = *self.local_portid.lock();
        if id != 0 { Some(Endpoint::Netlink(id)) } else { Some(Endpoint::Netlink(0)) }
    }
    fn remote_endpoint(&self) -> Option<Endpoint> { None }
    fn shutdown(&self, _h: u32) -> GeneralRet<()> { Ok(()) }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut q = self.recv_queue.lock();
        match q.front() {
            Some(data) => {
                let orig_len = data.len();
                let copy_len = orig_len.min(buf.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                q.pop_front();  // consume only after successful peek
                log::warn!("[netlink] try_recv: orig={} copied={} queue_rem={}", orig_len, copy_len, q.len());
                Ok(orig_len as isize)
            }
            None => {
                log::warn!("[netlink] try_recv: queue empty, returning EAGAIN");
                Err(SyscallErr::EAGAIN)
            }
        }
    }

    fn try_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        let n = self.try_recv(buf)?;
        Ok((n, Some(Endpoint::Netlink(0))))
    }

    fn try_peek_recvmsg(&self, buf: &mut [u8]) -> Result<(isize, Option<Endpoint>), SyscallErr> {
        let q = self.recv_queue.lock();
        match q.front() {
            Some(data) => {
                let orig_len = data.len();
                let copy_len = orig_len.min(buf.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                Ok((orig_len as isize, Some(Endpoint::Netlink(0))))  // return full message length for MSG_TRUNC
            }
            None => Err(SyscallErr::EAGAIN),
        }
    }

    fn last_recv_addr(&self) -> Option<Endpoint> {
        Some(Endpoint::Netlink(0))
    }

    fn try_send(&self, buf: &[u8], _f: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        self.try_sendmsg(buf, None, _f)
    }

    fn try_sendmsg(&self, buf: &[u8], _dest: Option<Endpoint>, _flags: crate::net::syscall::common::MsgFlags) -> Result<isize, SyscallErr> {
        log::warn!("[netlink] try_sendmsg: buf_len={}", buf.len());
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
            log::trace!(
                "[netlink] try_sendmsg processing msg type={} flags={:#x} len={}",
                u16::from_ne_bytes([buf[consumed + 4], buf[consumed + 5]]),
                u16::from_ne_bytes([buf[consumed + 6], buf[consumed + 7]]),
                nlmsg_len
            );
            route::handle_netlink_msg(&buf[consumed..consumed + nlmsg_len], self)?;
            consumed += crate::net::socket::netlink::netlink::nlmsg_align(nlmsg_len);
        }
        if consumed == 0 {
            return Err(crate::utils::error::SyscallErr::EINVAL);
        }
        log::trace!("[netlink] try_sendmsg done, consumed={}/{}", consumed, buf.len());
        Ok(buf.len() as isize)
    }

    fn socket_r_ready(&self) -> bool { !self.recv_queue.lock().is_empty() }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(&self.recv_wait)
    }

    fn push_netlink_message(&self, data: Vec<u8>) -> Result<(), SyscallErr> {
        if self.push_recv(data) {
            Ok(())
        } else {
            Err(SyscallErr::ENOBUFS)
        }
    }
    fn is_netlink_socket(&self) -> bool { true }
}
