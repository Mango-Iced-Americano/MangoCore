use crate::net::iface::DeviceKind;
use crate::net::syscall::common::MsgFlags;
use crate::net::{config::NET_INTERFACE, Endpoint, Socket, PSOCK};
use crate::timer::current_time_duration;
use crate::utils::error::{GeneralRet, SyscallErr, SyscallRet};
use alloc::sync::Arc;
use smoltcp::{phy::Device, phy::TxToken, time::Instant};

/// AF_PACKET packet socket (Linux AF_PACKET / PF_PACKET, family 17).
///
/// Provides raw access to the link-layer (ethernet) frame.
/// Minimal implementation: send only, no capture, no BPF, no promiscuous mode.
pub struct PacketSocket {
    /// Ethernet protocol type (ETH_P_* in network byte order).
    protocol: u16,
}

impl PacketSocket {
    pub fn new(protocol: u16) -> Self {
        PacketSocket { protocol }
    }
}

impl Socket for PacketSocket {
    fn bind(&self, _endpoint: &Endpoint) -> SyscallRet {
        Err(SyscallErr::EOPNOTSUPP)
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
        0
    }

    fn send_buf_size(&self) -> usize {
        0
    }

    fn set_recv_buf_size(&self, _size: usize) {}

    fn set_send_buf_size(&self, _size: usize) {}

    fn local_endpoint(&self) -> Option<Endpoint> {
        None
    }

    fn remote_endpoint(&self) -> Option<Endpoint> {
        None
    }

    fn shutdown(&self, _how: u32) -> GeneralRet<()> {
        Ok(())
    }

    fn try_recv(&self, _buf: &mut [u8]) -> Result<isize, SyscallErr> {
        // Capture not implemented yet.
        Err(SyscallErr::EAGAIN)
    }

    fn try_send(&self, buf: &[u8], _flags: MsgFlags) -> Result<isize, SyscallErr> {
        // The payload from sendto/sendmsg is a raw ethernet frame.
        // The destination address (sockaddr_ll) is passed separately as Endpoint.
        if buf.is_empty() {
            return Ok(0);
        }

        let timestamp = Instant::from_millis(current_time_duration().as_millis() as i64);
        let result = NET_INTERFACE.inner_handler(|inner| {
            // Send to the first real ethernet stack (eth0, ifindex=2), skip loopback.
            for stack in inner.stacks.iter_mut() {
                if stack.nic.kind() == DeviceKind::Ethernet && stack.nic.nic_id() == 2 {
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

    fn send_wait_queue(&self) -> Option<&crate::net::Mutex<crate::task::WaitQueue>> {
        None
    }
}
