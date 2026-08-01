//! TCP I/O 操作 —— 收发数据

use alloc::vec::Vec;
use smoltcp::socket::tcp;

use crate::utils::error::SyscallErr;

use super::inner::{with_tcp_mut, Established, Init, Inner};
use crate::net::config::NET_INTERFACE;

impl Inner {
    /// 非阻塞发送数据（适配 try_send 接口）
    pub fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        let ret = match self {
            Inner::Init(_) => Err(SyscallErr::EPIPE),
            Inner::Connecting(_) => {
                // 非阻塞 connect 后可能仍在此状态，外层 try_send 已调用 try_connect 过渡
                Err(SyscallErr::EAGAIN)
            }
            Inner::Listening(_) => Err(SyscallErr::EINVAL),
            Inner::Established(e) => e.send_slice(buf).map(|n| n as isize),
            Inner::SelfConnected(sc) => sc.send_slice(buf).map(|n| n as isize),
            Inner::Closed(_) => Err(SyscallErr::EPIPE),
        };
        ret
    }

    /// 非阻塞接收数据（适配 try_recv 接口）
    pub fn try_recv(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        let ret = match self {
            Inner::Init(_) => Err(SyscallErr::EINVAL),
            Inner::Connecting(_) => Err(SyscallErr::EAGAIN),
            Inner::Listening(_) => Err(SyscallErr::EINVAL),
            Inner::Established(e) => {
                with_tcp_mut(e.handle, |socket| {
                    if socket.can_recv() {
                        return socket
                            .recv_slice(buf)
                            .map(|n| n as isize)
                            .map_err(|_| SyscallErr::ENOTCONN);
                    }
                    let state = socket.state();
                    if state == tcp::State::CloseWait
                        || state == tcp::State::Closing
                        || state == tcp::State::LastAck
                        || state == tcp::State::TimeWait
                    {
                        return Ok(0);
                    }
                    if state == tcp::State::Closed {
                        return Err(SyscallErr::ECONNRESET);
                    }
                    // RST（如 listener close 时 abort backlog 连接）→ ECONNRESET
                    if state == tcp::State::Closed {
                        return Err(SyscallErr::ECONNRESET);
                    }

                    if !socket.may_recv() {
                        Ok(0)
                    } else {
                        Err(SyscallErr::EAGAIN)
                    }
                })
                .unwrap_or(Err(SyscallErr::EAGAIN))
            }
            Inner::SelfConnected(sc) => sc.recv_into(buf, false).map(|n| n as isize),
            Inner::Closed(_) => Ok(0),
        };
        ret
    }

    /// 带 MSG_PEEK 的接收（用于 recvfrom/recvmsg）
    pub fn try_recv_peek(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        match self {
            Inner::SelfConnected(sc) => sc.recv_into(buf, true).map(|n| n as isize),
            _ => self.try_recv(buf), // smoltcp 本身不支持 peek，暂简化
        }
    }
}
