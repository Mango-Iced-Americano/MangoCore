use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::phy::{Device, TxToken};
use smoltcp::time::Instant;

use crate::fs::vfs::event::EPollEvent;
use crate::fs::vfs::event::EventWaitQueue;
use crate::net::adapter::IfaceDevice;
use crate::net::config::NET_INTERFACE;
use crate::net::syscall::common::MsgFlags;
use crate::net::{Endpoint, Mutex, Socket, PSOCK};
use crate::net::{PacketEndpoint, PACKET_SOCKETS};
use crate::task::WaitQueue;
use crate::timer::current_time_duration;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};

pub const ETH_P_ALL: u16 = 0x0003;
pub const ETH_P_ARP: u16 = 0x0806;
pub const ETH_P_IP: u16 = 0x0800;

pub struct PacketSocket {
    pub inner: Mutex<PacketSocketInner>,
    recv_waiters: EventWaitQueue,
    send_waiters: EventWaitQueue,
}

pub struct PacketSocketInner {
    pub bound_ifindex: u32,
    pub bound_protocol: u16,
    pub rx_queue: VecDeque<Vec<u8>>,
    pub recvbuf_size: usize,
    pub sendbuf_size: usize,
}

impl PacketSocket {
    pub fn new(protocol: u16) -> Self {
        Self {
            inner: Mutex::new(PacketSocketInner {
                bound_ifindex: 0,
                bound_protocol: protocol,
                rx_queue: VecDeque::new(),
                recvbuf_size: 65536,
                sendbuf_size: 65536,
            }),
            recv_waiters: EventWaitQueue::new(),
            send_waiters: EventWaitQueue::new(),
        }
    }

    pub fn register_packet_socket(socket: &Arc<Self>) {
        crate::net::PACKET_SOCKETS
            .lock()
            .push(Arc::downgrade(socket));
    }

    pub fn is_bound(&self) -> bool {
        self.inner.lock().bound_ifindex != 0
    }
}

impl Drop for PacketSocket {
    fn drop(&mut self) {
        crate::net::PACKET_SOCKETS
            .lock()
            .retain(|w| w.upgrade().is_some());
    }
}

impl crate::net::Socket for PacketSocket {
    fn bind(&self, endpoint: &Endpoint) -> SyscallRet {
        match endpoint {
            Endpoint::Packet(ep) => {
                let mut inner = self.inner.lock();
                inner.bound_ifindex = ep.ifindex;
                inner.bound_protocol = ep.protocol;
                log::info!(
                    "[PacketSocket] bound to ifindex={} protocol=0x{:04x}",
                    inner.bound_ifindex,
                    inner.bound_protocol
                );
                Ok(0)
            }
            Endpoint::Unspecified => {
                // Bind to no specific interface (any)
                Ok(0)
            }
            _ => Err(SyscallErr::EINVAL),
        }
    }

    fn listen(&self) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn connect(&self, _endpoint: &Endpoint) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn accept(&self, _sockfd: u32, _addr: usize, _addrlen: usize) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
    }

    fn socket_type(&self) -> PSOCK {
        PSOCK::Raw
    }

    fn recv_buf_size(&self) -> usize {
        self.inner.lock().recvbuf_size
    }

    fn send_buf_size(&self) -> usize {
        self.inner.lock().sendbuf_size
    }

    fn set_recv_buf_size(&self, size: usize) {
        self.inner.lock().recvbuf_size = size;
    }

    fn set_send_buf_size(&self, size: usize) {
        self.inner.lock().sendbuf_size = size;
    }

    fn set_bind_to_device(&self, ifname: &str) -> SyscallRet {
        if ifname.is_empty() {
            self.inner.lock().bound_ifindex = 0;
            log::info!("[PacketSocket] unbound from device");
            return Ok(0);
        }
        let ns = crate::net::net_core::current_netns();
        let list = ns.device_list.lock();
        let iface = list.values().find(|d| d.iface_name() == ifname);
        match iface {
            Some(iface) => {
                self.inner.lock().bound_ifindex = iface.nic_id() as u32;
                log::info!(
                    "[PacketSocket] bound to device {} (ifindex={})",
                    ifname,
                    iface.nic_id()
                );
                Ok(0)
            }
            None => Err(SyscallErr::ENODEV),
        }
    }

    fn local_endpoint(&self) -> Option<Endpoint> {
        let inner = self.inner.lock();
        if inner.bound_ifindex != 0 {
            Some(Endpoint::Packet(PacketEndpoint {
                ifindex: inner.bound_ifindex,
                protocol: inner.bound_protocol,
                hatype: 1, // ARPHRD_ETHER
                pkttype: 0,
                halen: 6,
                addr: [0u8; 8],
            }))
        } else {
            None
        }
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        None
    }

    fn shutdown(&self, _how: u32) -> GeneralRet<()> {
        Ok(())
    }

    fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let mut inner = self.inner.lock();
        match inner.rx_queue.pop_front() {
            Some(frame) => {
                let len = frame.len().min(buf.len());
                buf[..len].copy_from_slice(&frame[..len]);
                Ok(len as isize)
            }
            None => Err(SyscallErr::EAGAIN),
        }
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        if buf.is_empty() {
            return Ok(0);
        }

        let ifindex = self.inner.lock().bound_ifindex;
        if ifindex == 0 {
            return Err(SyscallErr::ENETDOWN);
        }

        let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);

        let result = NET_INTERFACE.inner_handler(|inner| {
            for stack in inner.stacks.iter_mut() {
                if stack.nic.nic_id() as u32 == ifindex {
                    if let Some(tx_token) = stack.device.transmit(timestamp) {
                        tx_token.consume(buf.len(), |tx_buf| {
                            tx_buf.copy_from_slice(buf);
                        });
                        return Ok(buf.len() as isize);
                    }
                }
            }
            Err(SyscallErr::ENETDOWN)
        });

        match result {
            Some(Ok(n)) => Ok(n),
            Some(Err(e)) => Err(e),
            None => Err(SyscallErr::ENETDOWN),
        }
    }

    fn try_sendmsg(
        &self,
        buf: &[u8],
        dest: Option<Endpoint>,
        flags: MsgFlags,
    ) -> Result<isize, SyscallErr> {
        if let Some(Endpoint::Packet(ref pep)) = dest {
            if pep.ifindex != 0 {
                let saved = self.inner.lock().bound_ifindex;
                self.inner.lock().bound_ifindex = pep.ifindex;
                let result = self.try_send(buf, flags);
                self.inner.lock().bound_ifindex = saved;
                return result;
            }
        }
        self.try_send(buf, flags)
    }

    fn recv_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.recv_waiters.wait_queue())
    }

    fn recv_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.recv_waiters)
    }

    fn send_wait_queue(&self) -> Option<&Mutex<WaitQueue>> {
        Some(self.send_waiters.wait_queue())
    }

    fn send_event_queue(&self) -> Option<&EventWaitQueue> {
        Some(&self.send_waiters)
    }

    fn socket_r_ready(&self) -> bool {
        !self.inner.lock().rx_queue.is_empty()
    }

    fn socket_w_ready(&self) -> bool {
        let ifindex = self.inner.lock().bound_ifindex;
        ifindex != 0
    }

    fn recv_ready(&self) -> bool {
        self.socket_r_ready()
    }

    fn send_ready(&self) -> bool {
        self.socket_w_ready()
    }
}

pub fn deliver_frame_to_packet_sockets(frame: &[u8], ifindex: u32) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);

    let mut live_sockets: Vec<Arc<PacketSocket>> = Vec::new();
    let mut dead_indices = Vec::new();

    {
        let sockets = PACKET_SOCKETS.lock();
        for (i, weak_socket) in sockets.iter().enumerate() {
            match weak_socket.upgrade() {
                Some(socket) => {
                    let inner = socket.inner.lock();
                    let bound = inner.bound_ifindex;
                    let protocol = inner.bound_protocol;
                    let matches_iface = bound == 0 || bound == ifindex;
                    let matches_proto = protocol == ETH_P_ALL || protocol == ethertype;
                    if matches_iface && matches_proto {
                        live_sockets.push(socket.clone());
                    }
                }
                None => {
                    dead_indices.push(i);
                }
            }
        }
    }

    for socket in &live_sockets {
        let mut inner = socket.inner.lock();
        inner.rx_queue.push_back(frame.to_vec());
        // 先发布数据并释放 socket 状态锁，再通知等待者；消费者被唤醒后
        // 可以直接获取 inner，生产者也不会形成 inner -> waitqueue 嵌套锁。
        drop(inner);
        socket
            .recv_waiters
            .notify_events_at_most(EPollEvent::EPOLLIN | EPollEvent::EPOLLRDNORM, 1);
    }

    if !dead_indices.is_empty() {
        let mut sockets = PACKET_SOCKETS.lock();
        for &i in dead_indices.iter().rev() {
            if i < sockets.len() {
                sockets.remove(i);
            }
        }
    }
}

pub fn deliver_frames_from_veth_queue(ifindex: u32, rx_queue: &VecDeque<Vec<u8>>) {
    for frame in rx_queue.iter() {
        deliver_frame_to_packet_sockets(frame, ifindex);
    }
}
