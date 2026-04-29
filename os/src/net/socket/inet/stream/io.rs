//! TCP I/O 操作 —— 收发数据

use alloc::vec::Vec;

use crate::mm::UserBuffer;
use crate::utils::error::SyscallErr;

use crate::net::config::NET_INTERFACE;

use super::inner::{with_tcp_mut, Established, Init, Inner};

impl Inner {
    /// 非阻塞发送数据（适配 try_send 接口）
    pub fn try_send(&self, buf: &[u8]) -> Result<isize, SyscallErr> {
        let ret = match self {
            Inner::Init(_) => Err(SyscallErr::EINVAL),
            Inner::Connecting(c) => {
                // 握手未完成，不可发送
                if c.is_connected() {
                    Err(SyscallErr::EISCONN) // 已连接但没转 Established？不应该
                } else {
                    Err(SyscallErr::EAGAIN)
                }
            }
            Inner::Listening(_) => Err(SyscallErr::EINVAL),
            Inner::Established(e) => e.send_slice(buf).map(|n| n as isize),
            Inner::SelfConnected(sc) => sc.send_slice(buf).map(|n| n as isize),
            Inner::Closed(_) => Err(SyscallErr::EPIPE),
        };
        NET_INTERFACE.poll_until_quiescent();
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
                        socket
                            .recv_slice(buf)
                            .map(|n| n as isize)
                            .map_err(|_| SyscallErr::ENOTCONN)
                    } else if !socket.may_recv() {
                        Ok(0) // EOF
                    } else {
                        Err(SyscallErr::EAGAIN)
                    }
                })
                .unwrap_or(Err(SyscallErr::EAGAIN))
            }
            Inner::SelfConnected(sc) => sc.recv_into(buf, false).map(|n| n as isize),
            Inner::Closed(_) => Ok(0),
        };
        NET_INTERFACE.poll_until_quiescent();
        ret
    }

    /// 非阻塞接收数据到 UserBuffer（支持带偏移的 read_user）
    pub fn recv_to_user(
        &self,
        out: &mut UserBuffer,
        offset: usize,
        max_len: usize,
    ) -> Result<usize, SyscallErr> {
        match self {
            Inner::SelfConnected(sc) => {
                if offset > out.len() {
                    return Err(SyscallErr::EINVAL);
                }
                let available = core::cmp::min(out.len() - offset, max_len);
                if available == 0 {
                    return Ok(0);
                }
                let mut q = sc.buf.lock();
                if q.is_empty() {
                    if sc.send_shutdown.load(core::sync::atomic::Ordering::Acquire) {
                        return Ok(0);
                    }
                    return Err(SyscallErr::EAGAIN);
                }
                let n = core::cmp::min(available, q.len());
                let tmp: Vec<u8> = q.iter().take(n).copied().collect();
                out.write_at(offset, &tmp);
                for _ in 0..n {
                    q.pop_front();
                }
                Ok(n)
            }
            _ => {
                // 非 SelfConnected 走标准 try_recv
                let total = core::cmp::min(out.len().saturating_sub(offset), max_len);
                if total == 0 {
                    return Err(SyscallErr::EINVAL);
                }
                let mut tmp = alloc::vec![0u8; total];
                let n = self.try_recv(&mut tmp)?;
                if n > 0 {
                    out.write_at(offset, &tmp[..n as usize]);
                }
                Ok(n as usize)
            }
        }
    }

    /// 带 MSG_PEEK 的接收（用于 recvfrom/recvmsg）
    pub fn try_recv_peek(&self, buf: &mut [u8]) -> Result<isize, SyscallErr> {
        match self {
            Inner::SelfConnected(sc) => sc.recv_into(buf, true).map(|n| n as isize),
            _ => self.try_recv(buf), // smoltcp 本身不支持 peek，暂简化
        }
    }
}
